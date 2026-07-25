# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #36, #37, #38: stdlib argument handling (and #57, which is invalid)

Three small stdlib divergences, all about how optional and negative arguments
are interpreted. Verified against **both** Lua 5.2 and 5.4, which agree on all
three - so these are universal conformance bugs, not 5.4 tightenings.

### #57 is invalid - delete it, do not implement it

`notes/bugs.md` #57 claims two `tonumber` divergences. Both fail verification:

| expression | dellingr | Lua 5.2 | Lua 5.4 |
|---|---|---|---|
| `tonumber("+ff", 16)` | 255 | 255 | 255 |
| `tonumber(10, 16)` | error | 16 | error |

The first claim - that reference returns nil for a `+` sign with an explicit
base - is simply wrong; all three agree on 255. The second is real but points
the other way: dellingr matches **5.4** exactly, including the error message
("bad argument #1 to 'tonumber' (string expected, got number)"), and only 5.2
coerces. Since dellingr targets 5.4 semantics elsewhere, that is current
behaviour rather than a defect.

The audit was explicit that nothing in it had been verified. This is the case
where that mattered. Delete #57 and record why, so nobody re-derives it.

### #36 (Medium) - explicit `nil` is rejected for optional arguments

**Verified:** `("a.b"):find(".", nil, true)` raises
`bad argument #3 to <anonymous> (number expected, got nil)`. Both references
return `2`. That is the standard plain-find idiom, so this is the one most
likely to bite real scripts.

Reference `luaL_opt*` treats nil as "absent". The prevailing dellingr pattern
is `if num_args >= k { check_type(k, ...) }`, which errors on an explicit nil
instead. Affected: `string.find` init (`string.rs:268`), `string.sub` j,
`string.match` init, `string.gsub` n, `table.concat` sep/i/j, `table.sort`
comp, `table.unpack`/`unpack` i/j, and `table.insert` pos (its 3-arg form with
a nil pos errors differently from reference). `select` is already fine.

The codebase already has the right pattern applied inconsistently: `tonumber`
base and `table.remove` pos both guard with `state.typ(k) != LuaType::Nil`.

### #37 (Medium) - `table.concat`'s empty short-circuit ignores an explicit range

**Verified:** with `t[2] = "x"` and nothing else, `table.concat(t, "", 2, 2)`
returns `""` in dellingr and `"x"` in both references.

`src/lua_std/table.rs:224`: `if i > j || len == 0 { return "" }`. Reference
only defaults `j` from `#t`; an explicit `i..j` range is honoured regardless of
the border. Also, `i`/`j` go through `as usize`, so negative values saturate to
0 rather than addressing negative indices.

### #38 (Medium) - `unpack` / `table.unpack` truncate negative start indices

**Verified:** `select("#", table.unpack({10, 20}, -2, 2))` gives 3 in dellingr
and 5 in both references.

`src/lua_std/basic.rs:241` and `src/lua_std/table.rs:121` do
`state.to_number(2)? as usize`, saturating negatives to 0. So `unpack(t, -2, 2)`
returns `t[0], t[1], t[2]` instead of `t[-2]..t[2]` - wrong count *and* wrong
values.

The 255-result cap is a deliberate protocol limit and is not in question.

---

## Agreed implementation plan

Two further audit corrections from review: `string.find(".", nil, true)`
returns `2, 2`, not the `1, 1` bugs.md records; and **`table.insert` is not
part of #36** - all three implementations reject `table.insert(t, nil, 9)` at
argument 2, because the 3-arg form makes `pos` required. Drop it from the
finding and leave the code alone.

Separately noted, out of scope: dellingr's arg errors say `<anonymous>` where
reference names the function, because `check_type` sets `func_name: None`. That
is systemic and cosmetic, not part of these findings.

### The shared helper (#36)

Two internal helpers in `src/vm_aux.rs`:

- `State::is_none_or_nil(arg_number) -> bool`
- `State::check_optional_type(arg_number, expected) -> Result<bool>`

`check_optional_type` returns `false` for missing or nil, otherwise delegates
to the existing `check_type` and returns `true`. **That delegation is
load-bearing**: a wrong non-nil value must keep exactly the same `ArgError`,
argument index, expected/received types and label.

**Do not** add `opt_number` / `opt_string` style helpers yet - they would mix
optionality with coercion and integer-conversion policy and risk changing
non-nil behaviour, notably `table.concat`'s string-only separator.

Adopt at: `string.sub` arg 3; `string.find` arg 3; `string.match` arg 3;
`string.gsub` arg 4; `table.sort` arg 2; `table.concat` args 2, 3, 4;
`table.unpack` args 2, 3; global `unpack` args 2, 3. Also the two that already
handle nil correctly by hand - `table.remove` arg 2 and `table.move` arg 5 - so
the pattern is uniform. Use `is_none_or_nil` directly for `tonumber`, keeping
its existing precedence where argument 1 is validated before argument 2.

`string.find`'s `plain` argument already works, since `to_boolean(nil)` is
false.

Confirmed: `check_type` distinguishes missing from present, so an explicit nil
becomes `received: Some(LuaType::Nil)` and errors. No affected function bypasses
it.

### Integer conversion (#37, #38)

Generalize the existing `table_position` (`table.rs:368`) into a shared
`exact_integer_argument(state, arg, function_name)` in `src/lua_std.rs`, built
on `exact_i64` (`numeral.rs:5`), and reuse it from insert/remove/move, concat
and both unpack paths.

`unpack_values` (extract the duplicated global/table bodies into one private
helper, keeping thin separate registration closures so the snapshot Rust-fn ids
stay distinct and stable):

- keep `i` and `j` as `i64`;
- return zero results when `i > j`;
- `j.checked_sub(i)`, where overflow means "too many results to unpack";
- reject spans >= 255, preserving the existing exact 255-result maximum;
- only then iterate, converting keys to `f64`.

`table.concat`: keep signed `i64` endpoints and remove **only** `len == 0` from
the empty-result condition. An inclusive signed range avoids negative
truncation and needs no `j - i` arithmetic.

**No bound tied to `#t`.** Lua uses the border only to default `j`; explicit
endpoints may address any numeric keys, negative or far past the border, and a
missing element errors naturally. `table.concat`'s unbounded work is finding
#16, deliberately staged, and its remedy is per-element/output-byte charging -
**do not** fold that breaking cost-model change in here as a range clamp.

Note `exact_i64` also rejects fractions, infinities, NaN and out-of-safe-range
values. That matches 5.4; 5.2 accepts or truncates some, so those error cases
belong in Rust tests rather than the universal differential example.

### Tests

New `examples/stdlib_optional_args.lua`, **no `DIFF` marker** - all three
findings are universal, so unlike the previous loop no 5.4-forcing trick is
needed. Cover: all four string optional-nil cases; `table.concat`
separator/start/end defaults; `table.sort(t, nil)`; both unpack endpoint
defaults; a sparse explicit concat range outside `#t`; negative concat indices;
negative unpack ranges.

For global `unpack`, use `local compat_unpack = unpack or table.unpack` so
dellingr and 5.2 exercise the global while 5.4 produces identical output via
`table.unpack`.

Rust tests: `optional_type_treats_missing_and_nil_as_absent`;
`optional_type_preserves_non_nil_arg_error`; update
`unpack_rejects_huge_range_without_overflow` (`tests/call_counts.rs:100`) for
the exact-integer error; add `unpack_rejects_i64_span_overflow` using accepted
near-`i64` endpoints so `checked_sub` itself is exercised; exact-integer
rejection for fractional and out-of-range `table.concat` endpoints;
`table_insert_nil_position_is_required`; extend
`tonumber_uses_lua_numeral_grammar` with `tonumber("+ff", 16) == 255`; and a
`tonumber(10, 16)` test asserting argument 1, expected string, received number.

Those last two pin #57's measurements so the invalid finding is not
re-derived.

### Superseded questions

1. For #36, what is the right shared helper? Doing it call-site by call-site
   invites exactly the inconsistency that produced this bug - two functions
   already do it correctly and the rest do not. Is there an `opt_*` family that
   should exist in `vm_aux.rs` alongside `check_type`, and does adopting it
   change any error message or argument index for the *non*-nil cases?
2. `notes/bugs.md` flagged an unverified assumption behind #36: that
   `check_type(k, T)` errors on nil. My repro confirms it does for
   `string.find`. Confirm it holds for every listed function, since a couple
   may already route through a different path.
3. For #37 and #38, negative and out-of-range indices need care about
   overflow when converted. What is the correct conversion - the `exact_i64`
   helper the `table.move` fix now uses? And does honouring an explicit range
   in `concat` need a bound on how far past the border it will read?
4. Is `table.insert`'s 3-arg nil-pos case genuinely part of #36, or a different
   bug? bugs.md says it "errors differently than reference", which is vaguer
   than the rest.

Read `src/lua_std/string.rs`, `src/lua_std/table.rs`, `src/lua_std/basic.rs`,
and `check_type` plus its neighbours in `src/vm/vm_aux.rs`.
