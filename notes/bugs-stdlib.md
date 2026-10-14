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

## STD-023 - The string metatable's `__tostring` and `__call` are ignored

Source: lateral finding from the reviewer of the wave that gave strings a shared
metatable; confirmed against lua5.2 and lua5.4.

Strings now have the reference shared metatable, but `__tostring` and `__call`
dispatch still looks only at table metatables. In reference Lua, setting
`getmetatable("").__tostring` changes `tostring(s)`, and setting `__call` makes
`("x")(...)` callable. dellingr ignores both, so `getmetatable("")` returns a
real metatable whose fields only partly take effect.
