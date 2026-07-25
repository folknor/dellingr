# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #49, #50, #51, #52, #61: error and `tostring` taxonomy

Five small defects in how values are described to users, all in the same
area. All verified by execution against Lua 5.4.

| case | dellingr | Lua 5.4 |
|---|---|---|
| `print(setmetatable({}, {__tostring=function() return {} end}))` | prints `object: ObjectKey(10v1)` | error `'__tostring' must return a string` |
| `t = setmetatable({}, {__index="abc"}); type(t.upper)` | error `attempt to index a string value` | `function` |
| `local f = function() end; f.x` | `attempt to index a table value` | `attempt to index a function value` |
| `error("oops")` | `0:0: internal error: oops` | `input:1: oops` |
| `tostring({})` | `object: ObjectKey(7v1)` | `table: 0x...` |

### #49 (Low) - `print`/`tostring` accept a non-string `__tostring` result

`to_string_with_meta` (`src/vm/table_ops.rs:398-430`) stringifies whatever
`__tostring` returns. Its sibling `bytes_with_tostring_meta` (`434-467`)
already gets this right, checking `String | Number` and otherwise raising
`'__tostring' must return a string`.

`print` and the `tostring` builtin use the permissive one
(`lua_std/basic.rs:132, 223`). The fix is to fold `to_string_with_meta` onto
`bytes_with_tostring_meta` so there is one code path and one behaviour, rather
than duplicating the check.

### #50 (Low) - `__index` bottoming out in a string errors instead of chaining

`handle_index_metamethod_inner` (`src/vm/metamethod.rs:88-137`) accepts only
table / function / RustFn handlers. A string `__index` is legal in reference,
where indexing simply re-dispatches on the string value.

Note the interaction with the project's design: dellingr has no string
metatable, but `t.foo` on a table *does* fall back to the `table` library
(finding C-F1 in the coverage notes), and string values reach the string
library through their own path. So "re-dispatch on the string" has to mean
whatever `("abc").upper` already means in dellingr - confirm that before
implementing, and if the two cannot be made consistent, say so rather than
half-implementing it.

The error message is doubly wrong today: it says "attempt to index a table
value" (via `typ_simple`) for a *string* handler, which is #51.

### #51 (Low) - error messages misreport every object as a table

`Val::typ_simple` (`src/vm/lua_val.rs:89-98`) maps every `Obj` to
`LuaType::Table` "for display purposes". So indexing a Lua function reports
"attempt to index a table value".

The reason the shortcut exists is that `typ_simple` has no heap access. Every
call site that feeds an error message needs auditing: those with a heap in
scope should use `Val::typ(&heap)` instead. Loop 14 already did this for
`TypeError::Comparison` in `table_sort`, so the pattern is established.

**Do not** simply delete `typ_simple` if some call site genuinely has no heap;
in that case report the honest fallback rather than a confident wrong answer.

### #52 (Low) - script `error()` raises `ErrorKind::InternalError`

`src/lua_std/basic.rs:140-147` implements `error` with
`ErrorKind::InternalError(message)`, but `src/error.rs:97-99` documents that
variant as "corrupt bytecode or VM bug ... report these as bugs" and renders it
`internal error: <msg>`.

Two problems: an embedder filtering `InternalError` for crash reporting
receives ordinary script errors, and the rendering diverges from reference.
`RuntimeError`, or a dedicated `ScriptError` variant carrying the position
prefix, is the right shape.

### #61 (Medium) - `tostring` leaks a Rust `Debug` slotmap key

`impl fmt::Display for ObjectPtr` (`src/vm/object.rs:175-181`) writes
`object: {:?}` of the raw key because it has no heap access.
`Val::to_string_with_heap` (`lua_val.rs:108`) and `to_bytes_with_heap` (`120`)
both call it and both *do* have the heap.

Two defects in one line: the type word is always `object` rather than
`table`/`function`, and `ObjectKey(7v1)` exposes an internal representation.

**Determinism constraint:** the replacement cannot be a real address. The
digits are inherently divergent from reference and are not what matters; the
type word and the absence of Rust syntax are.

### Constraints

- Determinism is a product requirement - no addresses, no unseeded entropy.
- `unwrap_used` denied outside `#[cfg(test)]`; `HashMap`/`HashSet` banned;
  clippy denies warnings, so no dead code.
- Charge nothing new (#16 owns cost-model changes).
- Errors must leave the State consistent and reusable.

---

## Agreed implementation plan

Settled between the orchestrator and the deep reviewer. Two items grew beyond
the original findings; both are justified below rather than assumed.

### 1. One metamethod-aware conversion path (#49)

Make `to_string_with_meta` delegate to `bytes_with_tostring_meta`, which
already enforces `String | Number` and raises
`'__tostring' must return a string`. One code path, one behaviour. This fixes
`print` and `tostring` together (`basic.rs:132`, `219`).

### 2. Object rendering (#61)

Stop routing user-visible conversion through `ObjectPtr::Display`. Render as
`table: 0x{id:x}` / `function: 0x{id:x}`, taking the type from
`Val::typ(&heap)` and the digits from the **existing deterministic
`format_pointer_id` registry** (`table_ops.rs:469`, `vm.rs:178`) that `%p`
already uses.

Not the slotmap bits: reference renders `tostring(t)` and `string.format("%p", t)`
with the same digits, so a second identity scheme would be wrong on its own
terms as well as divergent.

**This changes the public API.** `State::to_string` (`stack.rs:175`) and
`to_bytes_coerce` (`196`) become `&mut self`, because minting an id needs
mutable access. Accepted deliberately: the crate is pre-1.0 and explicitly
unstable, and the alternative is either preserving the leak or adding interior
mutability purely for display. Call it out in the commit message.

### 3. Shared string-index helper (#50)

Add one slow-path helper that resolves any key against the current global
`string` table, and call it from all three places:

- the existing direct field slow path (`eval_index.rs:58`),
- generic string bracket indexing (`eval_index.rs:464`), which today rejects
  strings as non-tables,
- the `Val::Str` arm of `__index` chaining (`metamethod.rs:88`).

This is broader than finding #50 as written, and it is the *correct* scope:
`("abc")[1]` currently errors but is `nil` in reference, because reference's
string metatable points `__index` at the string table. Making the bracket path
agree with the field path removes a real divergence rather than adding one.

The no-string-metatable design is preserved - `getmetatable("abc")` stays nil.
String `__newindex` remains invalid.

`tests/metamethod_errors.rs:361` currently asserts that a string `__index`
handler *fails*; it must become a success test for `.upper`.

### 4. Heap-aware types everywhere (#51)

Replace all 36 `typ_simple` invocations with `typ(&heap)` and **delete
`typ_simple`** - leaving it would be dead code, which the lint gate denies.
Every site is inside a `State` method with `self.heap` or `state.heap` in
scope:

- `eval_store.rs`: 47, 62, 104, 125, 185, 236, 372, 435, 493, 494, 523 (11)
- `table_ops.rs`: 48, 117, 143, 163, 223, 228, 251, 268, 287 (9)
- `metamethod.rs`: 40, 122, 135, 160, 255, 269 (6)
- `eval_control.rs`: 67, 70, 73, 96, 98, 101 (6)
- `eval_index.rs`: 94, 477 (2)
- `eval.rs`: 247 (1)
- `stack.rs`: 172 (1)

### 5. `ErrorKind::ScriptError(String)` (#52)

`RuntimeError` is documented as "raised by a library operation" and
`InternalError` as a VM bug (`error.rs:95`); a script's `error()` is neither,
and hosts see `ErrorKind` through `Result` and `HostCallbacks`
(`host.rs:48`). Add a distinct variant carrying only the message - position
belongs to `Error`, not duplicated in the variant.

Internal blast radius is small: the only exhaustive match is `Display`
(`error.rs:268`); `is_recoverable` uses a non-exhaustive `if let` (`231`).
`tests/string_format.rs:273` asserts `InternalError` and must become
`ScriptError`.

### 6. Real error positions instead of `0:0:`

`State::error` unconditionally builds `(0, 0)` and carries a TODO
(`vm.rs:729`), while the exact line already exists in `Frame::current_line`
(`frame.rs:75`) and the same frame is in scope where the traceback is attached
(`eval.rs:383`). Populate a missing `line_num` there, before the traceback, so
named chunks render `input:1: oops` like reference. Parser errors keep their
existing line/column rendering.

This fixes the *default* position only. `error(msg, level)` - selecting a
caller frame, and level 0 meaning "no position" - stays with #58.

### Tests

Invalid table and boolean `__tostring` results through **both** `print` and
`tostring`, plus an accepted numeric result; string `__index` via dot and
bracket keys; function-valued arithmetic, concat, length, comparison and index
errors all naming `function`; `ScriptError` classification and named-source
line rendering; `tostring` giving a stable repeated identity, the right type
word, no `ObjectKey` substring, and digits agreeing with `%p`; and State reuse
after each new error path.

Read `src/vm/table_ops.rs` (`to_string_with_meta`, `bytes_with_tostring_meta`,
`format_pointer_id`), `src/vm/metamethod.rs`, `src/vm/eval_index.rs`,
`src/vm/lua_val.rs`, `src/vm/object.rs`, `src/vm/stack.rs`, `src/vm/frame.rs`,
`src/lua_std/basic.rs`, and `src/error.rs`.
