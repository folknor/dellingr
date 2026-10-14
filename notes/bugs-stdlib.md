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

Carried over from the `notes/todo.md` entry this replaces: the flat 1 for
`table.insert`/`table.remove` is recorded in a `src/lua_std/table.rs` comment
as a cost-model choice. The suggested charge is by shift distance
(`len - pos + 1`), as `table.move` charges per element. That changes
`cost_used` and invalidates saves, so it should be bundled with any other
pending `COST_MODEL_VERSION` bump (see VM-002) rather than shipped alone.

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
