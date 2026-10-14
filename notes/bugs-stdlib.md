# Bugs: standard library and pattern engine

Filed from the defect hunt at b83eab8, from two scopes: stdlib
(`src/lua_std.rs`, `src/lua_std/**`, `src/vm_aux.rs`) and patterns
(`src/patterns/**`). The patterns hunter checked reference behaviour with
`lua5.4 -e` / `lua5.2 -e` probes. The stdlib hunter did not run anything.
Stdlib findings whose mechanism lives in `src/vm/table.rs` or
`src/vm/lua_val.rs` and that were also found by a VM hunter are in
`notes/bugs-vm.md`: the O(n^2) `table.remove` / `table.sort`, the `pos == 0`
`table.remove`, and Rust functions rendering as `<function>`.

```
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
```

## STD-001 - Eager pattern compilation raises errors reference Lua never raises

Source: patterns (finding 1, the hunter's one real defect in the engine).

Claim broken: output matches reference Lua for everything implemented. No
`-- DIFF:` marks this, and neither the README nor `reference/` documents it.
Because errors kill the callback, a script that works in reference Lua dies
here.

Reference validates a pattern lazily: `match` only inspects the pattern text it
actually reaches. `classEnd`, `check_capture`, `capture_to_close`,
`start_capture`'s level check, and the `%b`/`%f` argument checks all run at
match time. `Compiler::compile` in `luapat.rs` validates the whole pattern up
front, and `string.find`/`match`/`gsub`/`gmatch` all call `from_bytes_try`
before matching. So a malformed tail behind a prefix that never matches errors
here and returns nil in reference:

| call | lua5.4 / lua5.2 | dellingr |
|---|---|---|
| `string.match('b', 'a)')` | `nil` | error "invalid pattern capture" |
| `string.gsub('b', 'a)', '')` | `b 0` | error |
| `string.find('abc', 'x(')` | `nil` | error "unfinished capture" |
| `string.find('abc', 'x%1')` | `nil` | error "invalid capture index %1" |
| `string.find('abc', 'x[')` | `nil` | error "missing ']'" |
| `string.match('b', 'a' .. ('()'):rep(40))` | `nil` | error "too many captures" |

`'x%1'` and `'x['` are more than a difference in when the error fires: reference
never errors on them for any subject that lacks an `x`. Errors that reference
raises on success (for example `'('` matching, then `get_onecapture` raising
"unfinished capture") are reproduced, because eager validation is a superset.
The divergence runs one way only: dellingr errors in more cases.

Hunter's direction:
- (a) Keep the compiled program, but make validation lazy by compiling an error
  into the item stream (`Item::Fail(PatternError)` at the first malformed point,
  raised when `match_at` reaches it). This keeps the compile-once structure and
  costs nothing on valid patterns, and it restores parity exactly, because every
  reference error site corresponds to a fixed pattern position. The "unfinished
  capture" case needs care: reference raises it only when pushing the results of
  a successful match, so an unclosed `(` must become a deferred "fail on success"
  flag, not an `Item::Fail` at the `(`.
- (b) Alternatively, declare eager validation a deliberate divergence in the
  README and add a `-- DIFF:` example.

Interaction with the compile-time/match-time error split that AGENTS.md calls
contract: (a) moves errors from `from_bytes_try` to `matches_bytes_from`. See
also STD-016, whose dead arms become reachable under (a).

## STD-002 - `gsub`/`gmatch` empty-match handling is Lua 5.2's while neighbouring surfaces follow 5.4, and no document says so

Source: stdlib (finding 9) and patterns (finding 2).

Claim broken: the `lua_std.rs` module doc lists the version-sensitive decisions
per function and does not mention these. Other surfaces are explicitly 5.4:
`string_format.rs` is "the Lua 5.4 `string.format` engine", and gmatch escapes a
leading `^` as 5.4 does.

`gsub` and `gmatch_iter` advance one byte after an empty match
(`new_pos = end + (start == end)`), which is the 5.2 behaviour. 5.4 uses
`lastmatch` to reject an empty match that ends where the previous match ended:
- `("abc"):gsub("%a*", "-")` gives `"--", 2` here (5.2) and `"-", 1` in 5.4.
- `gmatch("abc", "%a*")` yields `"abc"` and then `""` here (5.2), and only
  `"abc"` in 5.4.

`gsub`'s replacement-string handling follows 5.4, though: `%x` errors with
"invalid use of '%'", where 5.2 copies `x`. So `string` mixes versions in a way
nothing documents, and a script exercising both can match neither reference. The
diff harness accepts either reference per file, so this passes today. `gmatch`
also silently ignores 5.4's third `init` argument.

Direction (patterns hunter): pick one version (5.4 would be consistent with the
rest) and say so.

## STD-003 - Table library ignores `__len`

Source: stdlib (finding 2).

Claim broken: reference output matching. `__len` is implemented for `#`
(`eval_store.rs`), and both Lua 5.2 (`aux_getn` -> `luaL_len`) and 5.4
(`aux_getn` with `TAB_R`/`TAB_RW` -> `luaL_len`) honour `__len` in
`table.insert`, `table.remove`, `table.concat` and `unpack`/`table.unpack`.

`State::table_len` (`vm/table_ops.rs`) returns the raw border, so for a proxy
table with `__len`/`__index`, `#proxy` returns the metamethod's count but
`table.unpack(proxy)` returns nothing. No `-- DIFF:` or README entry covers
this.

## STD-004 - NaN sign leaks through `string.format`, so output differs across platforms

Source: stdlib (finding 3).

Claims broken: determinism. Also `math.rs`'s module doc: "Basic arithmetic
(`+ - * / %` ...) ... are bit-stable across platforms".

`format_float` (`string_format.rs`) takes the sign from
`number.is_sign_negative()` before checking for NaN. `0/0` and `math.sqrt(-1)`
produce the hardware default NaN. On x86-64 it has the sign bit set, so
`string.format("%f", 0/0)` prints `-nan`. On aarch64 it is positive, so the same
call prints `nan`. The constant folder runs `lua_div` on the compiling host, so
a folded `0/0` bakes that host's NaN bits into bytecode and snapshots.
`%e/%f/%g/%a` make the difference visible. `tostring` hides it, because Rust
prints `NaN` without a sign, and `%q` prints `(0/0)`.

Direction: canonicalise NaN to one sign in `format_float`, and either narrow the
`math.rs` bit-stability sentence to exclude NaN payloads and signs, or
canonicalise NaN in the arithmetic helpers.

## STD-005 - Number-to-string is Rust `Display`, not `%.14g`, and nothing durable records it

Source: stdlib (finding 4).

Claim broken: output matches reference except for `-- DIFF:` divergences. The
README's "Compatibility divergences" section lists only the zero-step `for` and
`math.log`, according to this hunter.

`vm/lua_val.rs::number_to_string` is `number.to_string()`, and every stdlib
coercion goes through it: `tostring`, `print`, `..` in `table.concat`, `%s`,
`string.len`/`upper`/`sub`/... on numbers, and `gsub` with a numeric
replacement. So `0.1+0.2` renders `0.30000000000000004` (reference `0.3`),
`1/3` renders `0.3333333333333333` (reference `0.33333333333333`), `1e15`
renders `1000000000000000` (reference `1e+15`), `1e300` renders as a
301-character integer string, and NaN renders as `NaN`. As a result,
`string.len(1/3)` returns 18 instead of 16, and string-keyed lookups built from
`tostring(x)` differ from reference. `examples/math.lua` carries
`-- DIFF: number_format` and `edge_cases.lua` has `nan_format`, so the test
surface knows. But this is the largest user-visible divergence, and no durable
document states it.

Direction: either implement `%.14g` (the existing `format_general` in
`string_format.rs` could back it) or document the divergence under
"Compatibility divergences".

## STD-006 - Table and basic library native work the README says is charged, but which is free or flat

Source: stdlib (finding 12). The O(n^2) `table.remove` / `table.sort` cases are
VM-008 in `notes/bugs-vm.md`.

Claim broken: README "Budget": "Data-dependent native work in the string,
pattern, and table libraries is charged: ... table elements visited".

- `unpack`/`table.unpack` charge nothing for visiting up to `MAX_RESULTS`
  elements, each a `get_table` that may call `__index`.
- `table.pack` charges a flat 1 for up to `MAX_RESULTS` stores.
- `table.insert` at position 1 shifts n keys for a cost of 1. The code documents
  this as deliberate, but the README sentence says the opposite.
- `select(n, ...)` is free and does `n` front `remove(1)` calls, which is O(n^2)
  stack shuffling. A single truncating move would do.

Direction: either spell out these exceptions in the README sentence, or change
the charges (with a `COST_MODEL_VERSION` bump).

## STD-007 - `string.format("%q", n)` matches neither reference for integral numbers

Source: stdlib (finding 5).

Claim broken: `lua_std.rs` and `string_format.rs` say `string.format` follows
Lua 5.4.

`quote_argument` always emits a hex float for finite numbers, so
`string.format("%q", 1)` gives `0x1p+0`. Lua 5.4 gives `1`, because the literal
is an integer printed with `LUA_INTEGER_FMT`. Lua 5.2 also gives `1`, because
`%q` coerces with `luaL_checklstring`.

Direction: with no integer subtype, the choice consistent with the rest of the VM
(see the `number_to_string` comment) is decimal for integral values in i64 range
and hex float otherwise.

## STD-008 - `_G` proxy aliases distinct non-UTF-8 string keys; its metatable is exposed and unprotected

Source: stdlib (finding 7).

Claim broken: string keys are byte strings everywhere else (the
`LuaType::String` doc says arbitrary byte strings, and `to_bytes` is exact).

`global_env_index`/`global_env_newindex` (`basic.rs`) convert the key with
`state.to_string(2)`, which is `String::from_utf8_lossy`, and `globals` is an
`IndexMap<String, Val>`. So `_G["\xff"] = 1`, `_G["\xfe"]` and `_G["\u{FFFD}"]`
all name the same global.

Direction: key `globals` by interned string pointer or `Vec<u8>`, or route
non-UTF-8 keys to the raw proxy path the way non-string keys already are.

Related smell: `getmetatable(_G)` returns the internal proxy metatable rather
than `nil` (reference `_G` has none). It is unprotected, so
`setmetatable(_G, nil)` or `getmetatable(_G).__index = f` silently breaks `_G`
for the rest of the State. Setting `__metatable` on it would close that.

## STD-009 - `tonumber(s, base)` trims different whitespace from `tonumber(s)`

Source: stdlib (finding 8).

Claim broken: reference matching, and consistency with the module's own shared
helper.

`parse_integer_with_base` (`basic.rs`) uses `<[u8]>::trim_ascii`, which does not
treat `\v` (0x0B) as whitespace. `parse_lua_numeral` uses
`numeral::is_lua_whitespace`, which does, as reference `isspace` does. So
`tonumber("10\v")` is `10` but `tonumber("10\v", 16)` is `nil`, where both
references give 16. Use `is_lua_whitespace`.

A lesser issue in the same function: accumulation happens in f64, so long digit
strings give a rounded float where 5.4 wraps a 64-bit integer.

## STD-010 - The module doc says `unpack` follows Lua 5.2, but it honours `__index` (5.4 behaviour)

Source: stdlib (finding 10).

Claim broken: the `lua_std.rs` module doc ("Global `unpack` follows Lua 5.2;
`table.unpack` uses the same implementation").

`unpack_values` reads with `state.get_table(1)`, which dispatches `__index`. Lua
5.2's `unpack` uses `lua_rawgeti`. Either the doc or the code is wrong.
`table.concat` also uses `get_table`, which is 5.4 behaviour (5.2 reads raw).

## STD-011 - `table.move` reads through `__index` but writes raw; keys collapse past 2^53

Source: stdlib (finding 11).

Claim broken: reference matching. `table.move` exists only in 5.3+, and 5.4 uses
`lua_geti`/`lua_seti` on both sides.

`table.rs` reads with `get_table` (metamethods) and writes with
`set_table_raw`, so a destination proxy's `__newindex` is bypassed. Integer keys
are built as `(f + i) as f64`, so beyond 2^53 distinct source and destination
keys collapse onto the same float key. With no budget configured,
`table.move(t, 1, 2^62, 2)` records the full count up front and then loops about
2^62 times. The hunter notes this is consistent with an unbudgeted State, but
worth knowing.

## STD-012 - Strings have no metatable, but string method calls work

Source: stdlib (finding 14). Related to VM-030 in `notes/bugs-vm.md` (string
methods resolve through the current global `string`), but a different mechanism.

Claim broken: reference matching. There is no `-- DIFF:` and no README entry.

`get_metatable_of` (`vm/table_ops.rs`, whose own doc says "we don't support type
metatables yet") returns nil for strings. So `getmetatable("")` is `nil`, and
the common `getmetatable("").__index` idiom errors, even though `("x"):upper()`
works through the string-method IC.

## STD-013 - `open_libs` is all-or-nothing only for stack overflow

Source: stdlib (finding 13).

Claims broken: the `STDLIB_SETUP_HEADROOM` comment ("keeps the operation
all-or-nothing ... part of the function registry"). AGENTS.md: "a rejected
operation must leave the stack, the string pool and the function registry
unchanged".

The preflight covers only the value stack. Under `snapshot`,
`register_lib_global_fn`/`register_lib_fn` fail with
`DuplicateFunctionRegistration` if the host already registered one of the ids
(`"print"`, `"string.find"`, `"string.gmatch.iter"`, ...) to a different
function. By then, earlier libraries are installed as globals, ids are
registered, strings are interned and tables are allocated. The failure leaves a
half-opened State.

Direction: validate all ids in a dry run before any mutation.

## STD-014 - `math.random(m, n)` loses precision and has modulo bias for wide spans

Source: stdlib (finding 15) and embedding (smaller item).

`math.random(m, n)` returns `as f64` of an i64 draw. For spans beyond 2^53 the
result is rounded: not uniform, and possibly outside `[m, n]` after rounding
(for example, `n = 2^63 - 1` rounds up to 2^63). `% span` also has modulo bias.
Neither is documented. Reference 5.4 returns integers here. The embedding hunter
notes that whether this is a defect depends on `arg_integer` accepting such
magnitudes, and that it should be checked against the math contract in
`src/lua_std/math.rs`.

## STD-015 - Smaller stdlib divergences

Source: stdlib (finding 15).

- `string.sub` validates argument 2 before argument 1, so `string.sub()` reports
  `bad argument #2 ... number expected`, where the references report argument #1
  (string expected).
- `table.concat` raises the generic `TypeError::Concat` ("attempt to concatenate
  a nil value"), where the references raise "invalid value (at index N) in table
  for 'concat'". The index is lost.
- Numeric-string coercion is inconsistent across the stdlib. `string.format`
  `%d/%f` accept `"10"` (`number_argument` parses it), but `math.*`, `select`,
  `string.sub` indices, `table.insert` positions, `tonumber(..., base)` and
  `math.random` reject it through strict `check_type(Number)`. The README only
  excludes coercion "in arithmetic and numeric control expressions".
- `print` and `error` convert their arguments to UTF-8 lossily. For `print` this
  follows the `on_print(&str)` API. For `error`, a byte-string message is
  silently altered, because `ScriptError::message` is a `String`.
- `string.find`/`match`/`gsub`/`gmatch`/`upper`/... copy the subject into a fresh
  `Vec` on every call (`string_or_number_bytes`). The copy is charged, but it is
  an avoidable allocation on the hottest string paths.

## STD-016 - Pattern engine dead arms and smaller items

Source: patterns (finding 3).

- `Item::Backref`'s `CapLen::Unfinished` arm returns
  `PatternError::UnfinishedCapture`, where reference raises "invalid capture
  index %N" at that site (`check_capture`). The arm is unreachable today because
  the compiler rejects backrefs to open captures, so it is dead code with a wrong
  message. If STD-001 moves validation to match time, it becomes reachable and
  the message must change.
- The pattern engine never constructs `PatternError::InvalidCaptureIndex(None)`
  (only tests in `errors.rs` do). The `Option` exists only for the shared
  `PatternErrorDetail` used by gsub replacement errors.
- `max_expand` takes a `min` parameter that every caller passes as `0` (`+`
  pre-advances `pos` instead). The `end == pos + min` break is just
  `end == pos`.
- `MatchState::captures` can return `UnfinishedCapture` on a successful match,
  but success traverses every item (no alternation, `$` only at the end), and the
  compiler rejects unclosed captures, so that path is unreachable. It is fine as
  a guard, but deserves an `expect`-style comment rather than a user-facing
  error.
- Compile-time host work is not proportional to the charged cost. Each `%x`
  escape and each bracket-class escape loops over all 256 bytes, and each class
  is interned through a `BTreeMap<[u64;4]>`. Callers charge `pattern.len()`, so a
  pattern of N escapes does about 256N uncharged work. This is bounded and within
  the README's "structural work is free" framing, so the hunter does not call it
  a cost-model defect, but a `const` 256-bit table per class letter would remove
  the loop.
- The empty-item fast path in `str_match` (pattern `""` or `"^"`) charges no
  matcher cost, whereas every other match charges at least one `charge()`. This
  is deterministic and harmless, but inconsistent with "matcher primitives each
  consume budget".
