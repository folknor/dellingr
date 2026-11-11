One finding.

**[P2] An interrupt refusal consumes host table operands before the Rust handler runs.** The defect starts at [eval.rs:450](/home/folk/Programs/dellingr/src/vm/eval.rs:450).

The broken claim is in [docs/limits.md](/home/folk/Programs/dellingr/docs/limits.md), under "Host table methods": `get_table` and `set_table` preserve their operands when metamethod dispatch fails before handler entry. `set_table`'s API documentation explicitly includes a handler call refused before entry.

`begin_call` sets the entry observer before calling `invoke_rust_fn`. That function can immediately return `Interrupted` from its [entry poll](/home/folk/Programs/dellingr/src/vm/eval.rs:662), without invoking the handler. The cleanup nevertheless sees "entered":

- [get_table cleanup](/home/folk/Programs/dellingr/src/vm/table_ops.rs:324) discards the key.
- [set_table cleanup](/home/folk/Programs/dellingr/src/vm/table_ops.rs:508) discards the key and value.

A concrete trigger is a host holding the table returned by:

```lua
return setmetatable({}, {__index = tostring, __newindex = rawset})
```

With that table alone at stack index 1:

- Push `"missing"`, trigger the interrupt, then call `get_table(1)`. It returns `Interrupted` with stack height **1**, although the promised height is **2**.
- Independently, push `"missing"` and `42`, trigger the interrupt, then call `set_table(1)`. It returns `Interrupted` with height **1**, although the promised height is **3**. No write occurs.

This prevents the host from resetting the interrupt and retrying with the preserved operands. Lua handlers preserve them because their observer is updated after admission succeeds.

The entry observer should be committed at the Rust invocation boundary, after its interrupt poll and before the callback runs.

This finding is established by tracing the entry and cleanup paths. Existing default-sweep runtime, host table atomicity, host set-table and direct Rust-entry interrupt tests pass; they do not cover this combination. No files were changed by this investigation.
