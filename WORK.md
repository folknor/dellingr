# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #27, #28: recursive traversals abort the host on deep data

Two traversals over the same object graph, both recursive with no depth bound,
both reachable from ordinary script data rather than hostile input. One loop
because they are the same defect shape over the same graph - though see the
question below about whether they genuinely share an implementation.

### #27 (Medium, but verified as a hard abort) - GC mark phase is recursive

`GcHeap::mark` (`src/vm/object.rs:352-363`) -> `mark_children` (`:370-386`) ->
`Table::mark_values` (`src/vm/table.rs`) -> `Val::mark_reachable` ->
`GcHeap::mark`, recursing once per level of nesting with no bound. Roughly 5
frames per level.

`MAX_CALL_DEPTH` and `MAX_METAMETHOD_DEPTH` protect the interpreter; nothing
protects the collector.

**Verified by running:**

```lua
local t = {}
for i = 1, 500000 do t = { t } end
```

Result: `thread 'main' has overflowed its stack / fatal runtime error: stack
overflow, aborting / Aborted (core dumped)`. Exit code 134. A plain Lua script,
costing ~2 per iteration and well inside any normal budget, kills the host
process uncatchably. Auto-GC fires during the loop, so it aborts before the
script even finishes.

This is also what currently blocks the "no malformed save can abort during
load" guarantee from the previous loop: `materialize_payload` ends with a
`gc_collect()` (`save_state.rs:601`), and a decoded save may legitimately
contain a deep table graph. Rejecting deep graphs is not an option - scripts
build them normally - so the collector has to stop recursing.

### #28 (Medium) - `SaveBuilder::encode_object` is recursive

`src/vm/save_state.rs:313-351`: `encode_object` -> `encode_val` ->
`encode_object`, once per nesting level. A script builds the chain cheaply, the
host then calls `save_state()`, and the native stack overflows inside an API
whose signature promises `Result<_, SaveError>`. Script-controlled data killing
the host during a host API call.

In practice #27 usually fires first with auto-GC enabled, which is why the
save-side abort is less visible - but a host that disabled auto-GC, or that
saves a graph built up just under the collection threshold, reaches this one.

### Constraints

- **Determinism.** Save output must stay byte-identical, which means the
  traversal order of `encode_object` must not change. A worklist that visits
  children in a different order would silently change every save file. GC order
  is not externally visible, but arena/id assignment order in the save
  absolutely is.
- `mark` is on the GC hot path and carries `#[hotpath::measure]`. AGENTS.md
  notes the recursion caveat for that attribute.
- The heap uses colours (`Color::Unmarked` / `Reachable`); an explicit worklist
  needs to keep the "already visited" test doing the same work it does now, or
  marking gets quadratic on shared subgraphs.
- `optimizations.md` #5 already proposes the iterative worklist for both.

### Questions for the reviewer

1. Do #27 and #28 genuinely share an implementation, or only a pattern? The
   previous loop's reviewer argued the three recursive problems (this pair plus
   the now-fixed `build_bytecode`) share a design pattern but not useful
   generic code, since GC works on `ObjectPtr` reachability and colour
   transitions while save encoding needs allocate/fill phases plus
   deterministic paths and diagnostics. Confirm or correct that. If they do not
   share code, say whether they should still land together.
2. For #27: explicit gray stack (`Vec<ObjectPtr>`) is the obvious shape. Where
   exactly does the worklist live - a scratch `Vec` on `GcHeap` reused across
   collections, or allocated per collect? Reused avoids a per-GC allocation but
   adds a field that must not be serialized and must be cleared on error paths.
   What does it cost on the `iter/pairs` and `alloc/closure` benches?
3. For #28: what is the exact current traversal order, and how do you preserve
   it byte-for-byte under an explicit stack? This is the part I would most
   expect to get silently wrong - an explicit stack naturally reverses child
   order unless the children are pushed in reverse. Is there an existing
   byte-stability test that would catch a regression here, and is it strong
   enough?
4. Is there a third recursive traversal over the same graph that I have not
   listed? `Val::mark_reachable`, `Table::mark_values`, and the `Markable` impls
   are the obvious neighbours, and the previous loop added
   `TransientRoots`/`SuspendedEnvironment` impls. Sweep for anything else that
   recurses per nesting level.
5. Does anything else in the crate recurse per level of *script-controlled*
   data depth, outside these traversals? The parser is now bounded and
   `build_bytecode` is bounded; I want to know whether this closes the class or
   just two instances of it.

Read `src/vm/object.rs`, `src/vm/table.rs`'s `mark_values`, the `Markable`
impls, and `SaveBuilder` in `src/vm/save_state.rs`.
