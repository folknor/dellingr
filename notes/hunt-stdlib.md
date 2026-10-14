# Hunt: stdlib (`src/lua_std*`, `src/vm_aux.rs`)

Read-only review at b83eab8. Findings ordered roughly by severity. Each names
the claim it breaks.

## 1. `table.remove` is O(n^2) wall work per call for a flat cost of 1

Claim broken: README "Budget" - "Data-dependent native work in the string,
pattern, and table libraries is charged: ... table elements visited". The
in-code justification in `table.rs` ("a mid-array call shifts every later
element one key at a time") also understates the real work.

`Table::array_remove` (`src/vm/table.rs`) forces `Map` storage (`ensure_map`)
and then, for every `i in pos..len`, does `map.shift_remove(&(i+1))` followed by
`map.insert(i, v)`. `IndexMap::shift_remove` is O(map size), and each reinsert
appends at the end, so the next key to remove sits near the front again. One
`table.remove(t, 1)` on an n-element table is therefore O(n^2), and it is
charged exactly 1 (`consume_cost(1)` in `table.rs`). A queue idiom
(`table.insert(q, x)` / `table.remove(q, 1)`) on a 1000-element table costs
about 10^6 map shifts per call at cost 1. That is a budget bypass on the
cost-bounded execution path. `table.remove(t)` (pop last) is also O(entries
after key `len` in insertion order), not O(1), whenever hash keys were inserted
after the array keys. As a side effect, every remove reorders `pairs` iteration
by moving the shifted keys to the end. The order is deterministic but
gratuitous.

`ensure_map` also turns every small inline table permanently into `Map`
storage on its first `table.remove`.

Structural fix: there is no array part. All integer keys live in one
`IndexMap`, which is the root cause of this, of the O(n) `array_insert`, and of
`get_array`/`set_array` copying through hashing in `table.sort`. A real array
part (Lua-style `Vec<Val>` plus a hash part) would make insert/remove a
`Vec::insert`/`Vec::remove` memmove, and would keep `pairs` order stable. If
the flat cost is kept, the wall-time work at least has to be linear.

## 2. Table library ignores `__len`

Claims broken: reference-output matching. `__len` is implemented for `#`
(`eval_store.rs`), and both Lua 5.2 (`aux_getn` -> `luaL_len`) and 5.4
(`aux_getn` with `TAB_R`/`TAB_RW` -> `luaL_len`) honour `__len` in
`table.insert`, `table.remove`, `table.concat` and `unpack`/`table.unpack`.
dellingr's `State::table_len` (`vm/table_ops.rs`) returns the raw border, so
`#proxy` and `table.unpack(proxy)` disagree for a proxy table with
`__len`/`__index`. `#proxy` returns the metamethod's count, but
`table.unpack(proxy)` returns nothing. No `-- DIFF:` marker or README entry
covers this.

## 3. NaN sign leaks through `string.format`, so output differs across platforms

Claims broken: the determinism claim, and `math.rs`'s module doc ("Basic
arithmetic (`+ - * / %` ...) ... are bit-stable across platforms").

`format_float` (`string_format.rs`) picks the sign with
`number.is_sign_negative()` before checking for NaN. `0/0` and `math.sqrt(-1)`
produce the hardware default NaN. On x86-64 that NaN has the sign bit set, so
`string.format("%f", 0/0)` prints `-nan`. On aarch64 the default NaN is
positive, so the same call prints `nan`. The constant folder runs `lua_div` on
the compiling host, so a folded `0/0` bakes that host's NaN bits into bytecode
and snapshots. The NaN-producing arithmetic is not bit-stable across platforms,
and `%e/%f/%g/%a` make the difference visible in script output. (`tostring`
hides it because Rust prints `NaN` without a sign. `%q` prints `(0/0)`.) Fix:
canonicalise NaN to one sign in `format_float`, and correct the math.rs
bit-stability sentence to exclude NaN payloads and signs, or canonicalise NaN in
the arithmetic helpers.

## 4. Number-to-string is Rust `Display`, not `%.14g`, and the README does not record it

Claim broken: "output matching reference Lua for everything it does implement
(except divergences marked `-- DIFF:`)", plus the README's "Compatibility
divergences" section, which lists only the zero-step `for` and `math.log`.

`vm/lua_val.rs::number_to_string` is `number.to_string()`. Every stdlib
coercion goes through it: `tostring`, `print`, `..` in `table.concat`, `%s`,
`string.len`/`upper`/`sub`/... on numbers, `gsub` numeric repl. So `0.1+0.2`
renders as `0.30000000000000004` (reference `0.3`), `1/3` as
`0.3333333333333333` (reference `0.33333333333333`), `1e15` as
`1000000000000000` (reference `1e+15`), `1e300` as a 301-character integer
string, and NaN as `NaN`. Consequences: `string.len(1/3)` returns 18 instead of
16, and string-keyed lookups built from `tostring(x)` differ from reference.
`examples/math.lua` carries `-- DIFF: number_format`, and `edge_cases.lua` has
`nan_format`, so the test surface knows about it. But this is the largest
user-visible divergence and nothing durable states it. Either implement `%.14g`
(the `format_general` code in `string_format.rs` already exists and could back
it) or document it under "Compatibility divergences".

## 5. `string.format("%q", n)` matches neither reference for integral numbers

Claim broken: lua_std.rs and string_format.rs say `string.format` follows Lua
5.4.

`quote_argument` always emits a hex float for finite numbers, so
`string.format("%q", 1)` gives `0x1p+0`. Lua 5.4 gives `1`, because a literal
`1` is an integer and is printed with `LUA_INTEGER_FMT`. Lua 5.2 also gives `1`,
because `%q` coerces with `luaL_checklstring`. With no integer subtype, the
choice consistent with the rest of the VM (see the `number_to_string` comment)
is: integral values in i64 range print as decimal integers, and everything else
prints as hex float.

## 6. `table.remove(t)` on an empty sequence ignores `t[0]`

Claim broken: reference-output matching.

Both references (5.2 and 5.4 `tremove`) do `geti(t, pos)` and then
`t[pos] = nil` even when `size == 0` and `pos == 0`. So with `t = {[0] = "x"}`,
`table.remove(t)` returns `"x"` and clears it. `Table::array_remove` returns
`Nil` for `pos == 0` without touching the table, so dellingr returns `nil` and
leaves `t[0]` in place.

## 7. `_G` proxy aliases distinct non-UTF-8 string keys

Claim broken: string keys are byte strings everywhere else (the
`LuaType::String` doc says arbitrary byte strings, and `to_bytes` is exact).

`global_env_index`/`global_env_newindex` (`basic.rs`) convert the key with
`state.to_string(2)`, which is `String::from_utf8_lossy`, and `globals` is
`IndexMap<String, Val>`. So `_G["\xff"] = 1` and `_G["\xfe"]` (or
`_G["\u{FFFD}"]`) name the same global. Fix: key `globals` by interned string
pointer or `Vec<u8>`, or route non-UTF-8 keys to the raw proxy path the way
non-string keys are.

Related smells:
- `getmetatable(_G)` returns the internal proxy metatable, not `nil` (reference
  `_G` has none). It is unprotected, so `setmetatable(_G, nil)` or
  `getmetatable(_G).__index = f` silently breaks `_G` for the rest of the
  State. Setting `__metatable` on it would close that.
- `_G.k = nil` (and any global set to nil) inserts a `Nil` entry into `globals`
  and never removes it (`set_global_value_owned`), so a script cycling through
  generated global names grows the map without bound. That is outside stdlib
  scope.

## 8. `tonumber(s, base)` trims different whitespace from `tonumber(s)`

Claim broken: reference matching, and the module's own shared helper.

`parse_integer_with_base` (`basic.rs`) uses `<[u8]>::trim_ascii`, which does
not treat `\v` (0x0B) as whitespace. `parse_lua_numeral` uses
`numeral::is_lua_whitespace`, which does, as reference `isspace` does. So
`tonumber("10\v")` is `10`, but `tonumber("10\v", 16)` is `nil`, where both
references give 16. Use `is_lua_whitespace`. Lesser issue in the same function:
the accumulation is in f64, so long digit strings give a rounded float where
5.4 wraps a 64-bit integer.

## 9. `gsub`/`gmatch` empty-match handling is Lua 5.2's, but the module claims per-function version choices and does not list this

Claim broken: the `lua_std.rs` module doc enumerates the version-sensitive
decisions per function and does not mention these two.

`gsub("abc", "%a*", "-")` returns `"--", 2` (5.2), where 5.4 returns
`"-", 1`. `gmatch("abc", "%a*")` yields `"abc"` and then `""` (5.2), where 5.4
yields only `"abc"`, because 5.4's `lastmatch` rejects an empty match that ends
where the previous match ended. `gsub`'s replacement-string handling, on the
other hand, follows 5.4: `%x` errors with "invalid use of '%'", while 5.2
copies `x`. So `string` mixes versions in a way the module doc does not state.
`gmatch` also silently ignores 5.4's third `init` argument.

## 10. Comment claims `unpack` follows Lua 5.2, but it honours `__index` (5.4 behaviour)

Claim broken: the `lua_std.rs` module doc ("Global `unpack` follows Lua 5.2;
`table.unpack` uses the same implementation").

`unpack_values` reads with `state.get_table(1)`, which dispatches `__index`.
Lua 5.2's `unpack` uses `lua_rawgeti`. Either the doc or the code is wrong.
`table.concat` also uses `get_table`, which is 5.4 behaviour (5.2 is raw).

## 11. `table.move` reads through `__index` but writes raw

Claim broken: reference matching. `table.move` exists only in 5.3+, and 5.4
uses `lua_geti`/`lua_seti` on both sides.

`table.rs` reads with `get_table` (metamethods) and writes with
`set_table_raw`, so a destination proxy with `__newindex` is bypassed. Integer
keys are also built as `(f + i) as f64`, so for indices beyond 2^53 distinct
source and destination keys collapse onto the same float key. With no
configured budget, `table.move(t, 1, 2^62, 2)` charges the full count up front
(the cost is recorded) and then loops about 2^62 times. That is consistent
with an unbudgeted State, but worth knowing.

## 12. More stdlib native work the README says is charged, but is free or flat

Claim broken: README "Budget" paragraph (as in finding 1).

- `unpack`/`table.unpack` charge nothing for visiting up to `MAX_RESULTS`
  elements, each a `get_table` that may call `__index`.
- `table.pack` charges a flat 1 for up to `MAX_RESULTS` stores.
- `table.insert` at position 1 shifts n keys for cost 1. The code documents
  this as deliberate, but the README sentence says the opposite.
- `select(n, ...)` is free and does `n` front `remove(1)` calls, which is O(n^2)
  stack shuffling. A single truncating move would do.

Either the README sentence needs these exceptions spelled out, or the charges
need to change (with a `COST_MODEL_VERSION` bump).

## 13. `open_libs` is all-or-nothing only for stack overflow

Claims broken: the `STDLIB_SETUP_HEADROOM` comment ("keeps the operation
all-or-nothing ... part of the function registry"), and AGENTS.md's "a rejected
operation must leave the stack, the string pool and the function registry
unchanged".

The preflight covers only the value stack. Under `snapshot`,
`register_lib_global_fn`/`register_lib_fn` fail with
`DuplicateFunctionRegistration` if the host already registered one of the ids
(`"print"`, `"string.find"`, `"string.gmatch.iter"`, ...) to a different
function. By that point earlier libraries have been installed as globals, ids
registered, strings interned and tables allocated. The failure leaves a
half-opened State. A dry-run validation of all ids, done before any mutation,
would make the claim true.

## 14. Strings have no metatable, but string method calls work

Claim broken: reference matching. There is no `-- DIFF:` and no README entry.

`get_metatable_of` (`vm/table_ops.rs`, whose own doc says "we don't support
type metatables yet") returns nil for strings. So `getmetatable("")` is `nil`,
and the common `getmetatable("").__index` idiom errors, even though `("x"):upper()`
works through the string-method IC.

## 15. Smaller divergences and smells

- `tostring(print)` and `string.format("%s", print)` render a Rust callback as
  `<function>` (`RUST_FN_DISPLAY`), while `%p` on the same value yields a
  `0x..` identity from `format_pointer_id`, and Lua closures render as
  `function: 0x..`. Reference prints `builtin: 0x...` / `function: 0x...` for
  both. Using `format_pointer_id` for `RustFn` in
  `bytes_with_default_string_coercion` would make it consistent.
- `string.sub` validates argument 2 before argument 1, so `string.sub()`
  reports `bad argument #2 ... number expected`, where the references report
  argument #1 (string expected).
- `table.concat` raises the generic `TypeError::Concat` ("attempt to
  concatenate a nil value"), where the references raise "invalid value (at
  index N) in table for 'concat'", and the index is lost.
- Numeric-string coercion is inconsistent across the stdlib. `string.format`
  `%d/%f` accept `"10"` (`number_argument` parses it), but `math.*`, `select`,
  `string.sub` indices, `table.insert` positions, `tonumber(..., base)` and
  `math.random` reject it via strict `check_type(Number)`. The README only
  excludes coercion "in arithmetic and numeric control expressions".
- `math.random(m, n)` returns `as f64` of an i64 draw, so for spans beyond 2^53
  results are rounded (not uniform, and possibly outside `[m, n]` after
  rounding, for example `n = 2^63 - 1` rounds up to 2^63). `% span` also has
  modulo bias. Neither is documented.
- `print` and `error` lossily UTF-8-convert their arguments. For `print` this
  follows the `on_print(&str)` API. For `error`, a byte-string message is
  silently altered, because `ScriptError::message` is a `String`.
- `string.find`/`match`/`gsub`/`gmatch`/`upper`/... copy the subject into a
  fresh `Vec` on every call (`string_or_number_bytes`). This is charged, but it
  is an avoidable allocation on the hottest string paths.
