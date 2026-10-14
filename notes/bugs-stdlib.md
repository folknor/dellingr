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

## STD-012 - Strings have no metatable, but string method calls work

Source: stdlib (finding 14). Related to VM-030 in `notes/bugs-vm.md` (string
methods resolve through the current global `string`), but a different mechanism.

Claim broken: reference matching. There is no `-- DIFF:` and no README entry.

`get_metatable_of` (`vm/table_ops.rs`, whose own doc says "we don't support type
metatables yet") returns nil for strings. So `getmetatable("")` is `nil`, and
the common `getmetatable("").__index` idiom errors, even though `("x"):upper()`
works through the string-method IC.

Design from a resolution-loop fixer, verified against lua5.4. It fixes this
entry and VM-030 together, but it needs `src/vm.rs` and snapshot code, which
that wave could not touch:
1. `State` gains `string_metatable: Option<ObjectPtr>`, rooted in
   `mark_gc_roots` with `GcHeap::mark` and created in `open_libs` right after
   the string library table is built, as a table with
   `__index = <string lib table>`. `with_restricted_env` leaves it alone.
2. `get_string_table_field_tracking_call` (`src/vm/eval_index.rs`) reads
   `string_metatable.__index` instead of `get_global("string")`. It keeps the
   table fast path, falls back to generic indexing when `__index` is a
   function, and rekeys `StringMethodCacheEntry` on the metatable's version
   instead of `globals_version`.
3. `get_metatable_of` (`src/vm/table_ops.rs`) returns `string_metatable` for
   strings.
4. Snapshots: `capture_env_tokens` adds the string metatable as a canonical env
   token (like `_G.metatable`), so it reloads as the same object. A script
   change to its `__index` needs a delta.
Reference facts: `getmetatable("").__index == string`; `setmetatable("x", {})`
errors (dellingr already refuses); `string = nil` does not break `("x"):upper()`;
assigning `getmetatable("").__index` takes effect. Both doc comments now state
the current behaviour.

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

## STD-021 - `error` with a byte-string message is altered by lossy UTF-8 conversion

Source: residue of the original stdlib hunt's smaller-divergences list. The
rest of that list was resolved; this item needs a change in `src/error.rs`.

`error(msg)` converts its message to UTF-8 lossily because
`ScriptError::message` is a `String`, so a message containing invalid bytes is
silently altered before the host sees it. The fixer's suggested shape: make
`ScriptError::message` hold bytes (`Vec<u8>`) with a lossy accessor for display,
and have `error` in `src/lua_std/basic.rs` read its argument through
`bytes_with_tostring_meta(1)`. That is a public API change.

The wave reviewer adds that `error()` and the public `to_string_with_meta()` also
build the lossy-expanded string without the size preflight `print` now does, so
invalid bytes can expand about threefold past the string cap before anything
checks it.
