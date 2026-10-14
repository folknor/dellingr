# Bugs: VM runtime, heap, persistence and host API

Filed from the defect hunt at b83eab8, from three scopes: vm-exec (`src/vm.rs`,
`src/vm/eval*.rs`, `frame.rs`, `stack.rs`, `metamethod.rs`, `cost_meter.rs`,
`limits.rs`, `error.rs`), vm-heap (`object.rs`, `lua_val.rs`, `table.rs`,
`table_ops.rs`, `anchor.rs`, `save_state/**`) and embedding (`lib.rs`,
`host.rs`, `main.rs`, `rng.rs`, the public `State` surface). Where one finding
came from more than one scope, the entry says so. None of the hunters ran
anything.

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

## VM-022 - The host `get_table` disagrees with the `t[k]` opcode on string receivers

Source: vm-exec (finding 8, LOW).

`State::get_table` goes through `get_table_with_key`, which raises
`TableIndex(string)` for a string receiver. `OP_GET_TABLE` / `OP_GET_FIELD`
resolve string receivers through the `string` library. The method's name and doc
("As in Lua, this function may trigger a metamethod for the index event") promise
`t[k]` semantics, so for `("abc")["upper"]` the host sees a different result
from the script.

## VM-023 - `State::concat(n < 2)` reports the wrong kind of error

Source: vm-exec (finding 9, LOW).

`concat` rejects `n < 2` with
`ArgError { arg_number: 1, func_name: "concat", detail: TypeMismatch, expected: None }`,
which renders as "bad argument #1 to 'concat' (value expected)". The problem is a
count, not a type or a missing value, and argument #1 is not a Lua argument.
`ArgDetail::TypeMismatch`'s own doc ("Type or presence mismatch, described by
expected and received") does not fit. `InvalidStackIndex` or `OutOfBounds` would
be accurate.

## VM-034 - `set_cost_budget` from inside a callback interacts with the dispatch loop's pending cost

Source: residue of the error-path cost-flush fix, from a resolution-loop fixer.

`set_cost_budget` resets `cost_used` to 0 while an outer `Frame::eval` still
holds unflushed batched opcode cost, which is then charged against the new
budget. So `cost_used` around a mid-run budget reset depends on the batching
interval, which the `COST_CHECK_INTERVAL` doc says it never does. The doc for
`set_cost_budget` does not say whether a mid-run reset is supported. Either
flush the pending batch before the reset or document mid-run resets as
unsupported.

## VM-044 - `encode_env_deltas` is quadratic per environment table

Source: residue of a smaller-heap-items entry from the original hunt; a
resolution-loop fixer confirmed it still holds.

`encode_env_deltas` (`src/vm/save_state.rs`) runs `live.iter().any(...)` and
`baseline.entries.iter().find(...)` / `.any(...)` in its deleted-key loop, its
upsert loop and the `replayed_order` computation, which is O(baseline x live)
per environment table. Scripts can grow `string`/`math`/`table` without bound,
so save time is quadratic in user additions to library tables. Fix: build a map
of baseline entries and a set of live keys once per table, keyed so equality
matches runtime key equality. Same residue: allocators note that callers must
check `is_full()` and run GC, while snapshot materialization deliberately
allocates without collecting. The notes now name that exception, and a typed
"allocation without collection" token would make it explicit.

## VM-045 - The `to_string_with_meta` host API converts without a size preflight

Source: lateral finding from the fixer that added `print` and `error` size
preflights.

`State::to_string_with_meta` (`src/vm/table_ops.rs`) calls
`String::from_utf8_lossy` on the full byte vector. Invalid bytes expand
threefold to U+FFFD, so the result can exceed `MAX_STRING_BYTES` before anything
checks it. `print` and `error` now compute the lossy length (`lossy_utf8_len` in
`src/lua_std/basic.rs`) and check it with `checked_string_growth` before
converting. The host API should do the same, with the helper moved somewhere
both can reach.
