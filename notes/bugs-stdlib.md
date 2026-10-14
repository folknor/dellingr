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
- Explicit `table.concat(t, sep, i, j)` / `unpack(t, i, j)` ranges beyond
  +/-2^53 can still collapse distinct integer keys onto one f64 key, unlike
  `table.move`, `insert` and `remove`, which now reject such ranges. (Lateral
  finding from a resolution-loop reviewer.)

## STD-018 - `table.sort` ignores `__len`, `__index` and `__newindex`

Source: lateral finding from a resolution-loop fixer.

Claim broken: the `src/lua_std.rs` module doc now says the table library follows
Lua 5.4's metamethod rules, with `table.sort` named as the exception that works
on the raw array. Lua 5.4's `sort` uses `aux_getn` (honouring `__len`) and
`lua_geti`/`lua_seti`, so a proxy table sorts in reference Lua and not here.
The exception is documented, but no `-- DIFF:` marker or README entry records
it. `table.sort` goes through the crate-internal `table_sort` in
`src/vm/table_ops.rs`. A comment at the sort site in `src/lua_std/table.rs` notes
the gap. The same fixer suggests that a shared metamethod-aware length helper
belongs in `table_ops.rs` beside `table_len`, instead of the library-local
`sequence_len`.

## STD-020 - Bytecode `#` passes only one operand to `__len`

Source: lateral finding from a resolution-loop reviewer.

Lua 5.4 calls `__len(t, t)` (two operands). The table library's length helper
now does too, but the bytecode `#` operator (`eval_store.rs`) passes only one.
A `__len` that inspects its second argument behaves differently under `#t` and
under `table.insert`.

## STD-019 - `string.format` rejects `%F`

Source: lateral finding from a resolution-loop fixer.

Claim broken: `string_format.rs` is "the Lua 5.4 `string.format` engine". Lua
5.4's `lstrlib` accepts the conversions `aAeEfFgG`. dellingr rejects `%F`.
