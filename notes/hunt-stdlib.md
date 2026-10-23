# Hunt: stdlib (non-string standard library)

Scope: `src/lua_std.rs`, `src/lua_std/basic.rs`, `src/lua_std/table.rs`,
`src/lua_std/math.rs`, and their use of the VM machinery. Read-only review;
nothing was built or run. Reference-Lua behaviour quoted below comes from the
5.2.4 / 5.4 C sources (`lbaselib.c`, `lmathlib.c`, `ltablib.c`), not from
running the interpreters.

The yardstick for most findings is `docs/compatibility.md`'s opening claim:
"Every deliberate divergence from Lua 5.2 and 5.4. A difference not listed
here is a bug. Features that are absent altogether are listed in the README
under 'Won't implement'." Where a finding is only a doc gap, the fix can be
either a code change or a new line in that file, but as things stand the
claim is false.

---

## 1. The `_G` proxy's raw contents split from the real globals (medium)

`basic.rs` `open_base` makes `_G` an empty table whose protected metatable
has `__index` / `__newindex` handlers that forward to `GlobalEnv`. Only the
metamethod paths forward. The raw paths read and write the proxy table
itself:

- `pairs(_G)` / `next(_G)` iterate the proxy, which is empty, so they return
  no globals at all. In both reference versions they enumerate every global.
- `rawget(_G, "print")` returns nil.
- `rawset(_G, "x", 1)` stores `x` in the proxy table, not in the global
  environment. After that:
  - `x` (a bytecode global read) is nil;
  - `_G.x` returns 1, because the raw hit means `__index` never fires;
  - a later `x = 2` changes the global, but `_G.x` keeps returning 1;
  - `_G.x = 3` hits the raw key, so `__newindex` never fires. It updates the
    proxy and leaves the global alone.

  The script now has two different values both named `x`, and nothing
  reconciles them. Snapshots save the proxy table as an environment delta
  (`docs/save-state.md`), so the split survives a save/load.
- The proxy is the same table in every `with_restricted_env` environment.
  If a sandbox's whitelist includes `_G`, a raw key written there inside a
  restricted callback stays visible after the restriction ends. Any
  non-UTF-8 string key does the same, because `utf8_global_key` routes those
  into the proxy table.

What the docs say: compatibility.md lists only the `getmetatable(_G)` result
and the `setmetatable(_G, ...)` error as `_G` divergences. Empty `pairs(_G)`,
nil `rawget(_G, k)` and the `rawset` split are unlisted, so by that file's
own claim they are bugs. They also fall under AGENTS.md's "surprising
behaviour" test, since a script that writes through `rawset(_G, ...)` (a
common idiom for defining globals under a strict-mode `__newindex`) silently
defines nothing.

Structural options:
- Make the global environment a real table, so `_G` is that table and the
  `GlobalEnv` builtin mirror and versioning are layered over it.
- Or make the raw primitives proxy-aware: `rawget`, `rawset`, `next` and
  `pairs` special-case the proxy and route UTF-8 string keys to `GlobalEnv`.

The second option is a patch over a leaky abstraction. The first removes the
class of bug.

## 2. An explicit nil in an optional argument is rejected (low-medium)

Three functions check whether an optional argument is present by looking at
`get_top()` instead of using `is_none_or_nil`:

- `error(msg, nil)` (basic.rs, the `error` closure): `get_top() >= 2`, so
  `arg_integer(2)` fails on the nil and raises `bad argument #2 to 'error'
  (number expected, got nil)`. Both references read the level with
  `luaL_optint`/`luaL_optinteger(L, 2, 1)`, so nil means level 1. The script
  author's own error message is replaced by an argument error, which is the
  worst case: the message that explains the failure is lost.
- `math.log(x, nil)` (math.rs `log`): `get_top() >= 2` followed by
  `check_type_named(2, Number)` raises an error. Both references check
  `lua_isnoneornil(L, 2)` and return the natural log.
- `math.atan(y, nil)` (math.rs `atan`): same pattern. 5.4 uses
  `luaL_optnumber(L, 2, 1)`, and 5.2 ignores the second argument.

What the docs say: none of these appear in compatibility.md. The module doc
of `src/lua_std.rs` states the library's convention: "An optional argument
that is absent or nil takes its default". `opt_integer`,
`check_optional_type_named` and `tonumber`'s `is_none_or_nil` all follow it.
These three paths do not. The likely trigger is forwarding a possibly-nil
value, as in `error(msg, lvl)` or `math.log(x, base)`.

Fix: `error` should use `opt_integer(state, 2, "error")` with a default of 1,
and `log` and `atan` should use `check_optional_type_named`.

## 3. `table.concat` rejects a number separator (low)

`table.rs` `concat`: `check_optional_type_named(state, 2, LuaType::String,
"concat")` raises `string expected, got number` for `table.concat(t, 0)`.
Both references read the separator with `luaL_optlstring`, which converts a
number to its string, so they return `"102"` for `{1, 2}`. The elements in
the same function do accept numbers (`bytes_with_default_string_coercion`).

The "Coercion" section of compatibility.md covers only string-to-number
coercion. Number-to-string coercion of the separator is not listed anywhere,
so this is an unlisted divergence.

## 4. Other unlisted divergences in scope (low)

None of these is listed in compatibility.md:

- `pairs` ignores `__pairs` (`base_pairs` pushes `next` without consulting
  the metatable). Both 5.2 and 5.4 call `__pairs` when it is present. The
  "Globals and metatables" section lists which metatable fields are
  implemented, which only implies that `__pairs` is not.
- `ipairs` reads through `__index` (`base_ipairs_iter` uses
  `get_table_unguarded`). This matches 5.4, but 5.2's `ipairsaux` uses
  `lua_rawgeti`. The table-library bullet that lists 5.4 metamethod
  semantics does not mention `ipairs`.
- `math.random(0)` raises `interval is empty`. In 5.4, `math.random(0)`
  returns a random integer covering all 64 bits. The module doc of
  `src/lua_std.rs` claims "`math.random` follows Lua 5.4's interval and error
  behavior", and this case breaks that claim. Producing a full-width integer
  makes little sense with f64-only numbers, so the fix is probably a doc
  line, but as written the claim is wrong.

## 5. Two docs overstate table-library charging, and the metamethod insert/remove loop has no cost bound (low-medium; doc defect plus a lateral hazard)

`README.md` ("Budget"): "Data-dependent native work in the string, pattern,
and table libraries is charged: bytes examined or emitted, table elements
visited ...". `docs/compatibility.md` ("Cost-related differences"): "Native
string, pattern and table work is charged per byte or element."

Both statements are false for `insert`, `remove` and `pack` (flat 1) and for
`unpack` (free). `docs/budget.md` documents all four as deliberate. The two
general statements need the exceptions spelled out.

The gap matters most in the metamethod paths of `insert` and `remove`
(`insert_through_metamethods`, `remove_through_metamethods`). The shift loop
runs `first_empty - pos` or `size - pos` iterations, and that count comes
from a script-controlled `__len`, not from storage that exists. Example:

```lua
local t = setmetatable({}, {__len = function() return 2^53 end})
table.insert(t, 1, 0)
```

This passes `check_sequence_keys` (the range ends at 2^53, inside the
inclusive bound). It then runs about 9e15 iterations of raw get plus raw set
of nil inside one native call. The total charge is 1, plus the `__len`
call's opcodes. A table with only `__len` takes the metamethod path but
never re-enters Lua in the loop, so no opcode budget check ever runs.

The raw path cannot do this, because its length is bounded by real array
storage. The other walks are all bounded: `sort` charges `len` up front and
caps it at `i32::MAX`, `move` charges per element, `concat` charges per
element, and `unpack` caps at 255.

Under budget.md this is a performance concern, not a cost-model defect.
`while true do end` already shows the budget is not a termination
guarantee. Still, it is the one place where a single library call with a
flat charge does work chosen by the script with no bound. If the flat
charge stays, consider capping the metamethod-path shift distance (for
example at `TABLE_SORT_MAX_LEN`, as `sort` does) so that one call is
bounded.

## 6. `rawset` writes a table for free while `t[k] = v` costs 1 (low-medium; cost-model gap)

`TableWrite` opcodes charge 1 (`docs/budget.md`, "Static opcode charges").
`rawset(t, k, v)` performs the same write and charges nothing, since `Call`
is 0 and `rawset` never calls `consume_cost`. A script can therefore fill
tables without paying the `TableWrite` cost by writing `rawset(t, i, v)`
instead of `t[i] = v`.

budget.md has a "Free by design" list (`print`, `select`, `unpack`,
`string.len`, `tostring`) and a "Native charges" list. `rawset` is in
neither. The same is true of the rest of the base library: `type`,
`tonumber`, `rawget`, `rawequal`, `rawlen`, `next`, `pairs`, `ipairs` and
its iterator, `getmetatable`, `setmetatable` and `error`. For all of these
except `rawset`, being free plausibly matches the design, because reads are
uncharged opcodes too. `rawset` is the exception: it is a free version of a
charged operation. That reads as an oversight unless it is documented as
deliberate.

Either charge `rawset` 1 (a `COST_MODEL_VERSION` bump) or add it, and the
rest of the base library, to "Free by design".

## 7. The README "Won't implement" list is incomplete (low; doc defect)

compatibility.md delegates the list of absent features to the README. The
README lists coroutines, io/os/debug, pcall/xpcall/assert, goto, `//`,
bitwise operators, `string.rep/byte/char`, arithmetic, comparison and concat
metamethods, implicit coercion, and long strings. These are absent from this
scope's libraries but not listed:

- `load`, `loadstring`, `dofile`, `require`, `collectgarbage`;
- `math.randomseed` (the host's `set_rng_seed` replaces it), `math.type`,
  `math.tointeger`, `math.ult`, `math.maxinteger`, `math.mininteger`, and
  5.2's `math.pow`, `ldexp`, `frexp`, `cosh`, `sinh`, `tanh`, `log10`;
- `table.maxn`;
- the metatable fields `__pairs`, `__gc`, `__mode`, `__close` and `__name`.

`__mode` matters most: a script that builds a weak-keyed cache with
`setmetatable({}, {__mode = "k"})` gets no error and a cache that never
releases its entries.

## 8. Stale comment on global `unpack` (trivial)

In `basic.rs`, the comment above `add!("unpack", base_unpack_values)` says
"Defaults to 1..#list, matching Lua 5.2's global unpack." The implementation
(`lua_std::unpack_values`) follows 5.4: the length comes from `__len` and
reads go through `__index`, which the doc comment on `unpack_values` itself
states. The comment should describe the 5.4 semantics or simply point to
`unpack_values`.

---

## Checked and found sound

- Stack preflights: `open_libs` reserves 4 slots, and that covers the deepest
  overlap in `open_base` (`_G`, its metatable, the protection key and value).
  `unpack` pushes at most 255 values, each through a checked push. The
  iterator helpers are net-neutral or push through checked paths.
- 1-based indexing in every function. After a `__len` call or an `__index`
  read, the argument slots are where the code expects them
  (`insert_through_metamethods`' `push_value(3 or 2)`, and
  `remove_through_metamethods` after `set_top(1)`).
- GC: the scratch table in `sort_through_metamethods` is rooted on the stack.
  The comparator path of the raw `table_sort` roots a clone of the detached
  array. `pack`, `concat` and `tostring` allocate only while their inputs are
  on the stack.
- `table.move`'s batched `consume_units` charge reproduces the per-element
  loop exactly (`cost_meter.rs`). Its copy direction matches 5.4's `tmove`
  condition.
- Raw `remove` and `insert` position rules match 5.4, including `pos = 0` on
  an empty table (returns `t[0]` and clears it). The `__len` wrapping checks
  match the doc in `lua_std.rs`.
- `tonumber(s, base)` matches 5.2's `luaB_tonumber` byte for byte: whitespace
  set, sign handling, and rejection of interior spaces.
- `math.modf` and `math.min`/`max` behave correctly at the inf, NaN and
  signed-zero edges. `math.random(m, n)` stays within `[m, n]` after f64
  rounding, as its comment argues. `deg` and `rad` multiply by a constant,
  which is bit-stable.

## Lateral notes (outside the defect bar)

- Performance: `utf8_global_key` allocates a `String` (`name.to_owned()`) on
  every `_G.k` read or write that reaches the proxy handlers. A
  borrow-then-lookup that does not allocate, or interned-string keyed global
  access, would remove it.
- Performance and consistency under `snapshot`: `base_ipairs` calls
  `push_lib_fn("ipairs.iter", ...)`, which runs `register_rust_fn` (a string
  hash lookup) on every `ipairs(...)` call, although `open_base` already
  registered the id. `base_pairs` uses a plain `push_rust_fn(base_next)` and
  relies on the reverse address lookup. Pushing the already-registered
  function directly in both would be cheaper and consistent.
- `math.fmod` (and `numeral::lua_modulo`, the VM's `%`) use Rust's f64 `%`,
  which LLVM lowers to a libm `fmod` call on most targets. `fmod` is exact,
  so every conforming libm agrees and replays are safe. Still, it is a
  host-libm dependency that `docs/determinism.md` does not mention next to
  its "computed in-crate, never by the platform libm" wording.
- `VmRng::random_range_i64` returns early without drawing when `end <= start`,
  so `math.random(1)` and `math.random(m, m)` do not advance the stream. 5.4
  always draws. This is deterministic and fine, but it is a replay-visible
  property of the RNG draw sequence that `docs/determinism.md` does not
  state. If it ever changed, every replay that hits a degenerate range would
  diverge.
- `table.sort` raw path: a comparator's writes to `t[1..n]` are overwritten
  by the final `set_array` writeback, and a metatable the comparator adds
  mid-sort is bypassed by that raw writeback. Reference Lua's behaviour here
  is unspecified too, so this is not a defect. compatibility.md's sort bullet
  documents the call-order difference only for the metamethod path, not this
  one.
