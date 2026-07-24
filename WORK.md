# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #5, #23 (and the stale TODO entry #55): `RustFn` identity

Both are about how a `Val::RustFn` is compared, hashed, and rendered. Same
file, same values, one loop.

Both verified by running, against reference Lua 5.4 on the same script:

| expression | dellingr | reference 5.4 |
|---|---|---|
| `print == print` | `false` | `true` |
| `local f = print; f == print` | `false` | `true` |
| `rawequal(print, print)` | `false` | `true` |
| `t[print] = 1; t[print]` | `nil` | `1` |
| `format("%p", print) == format("%p", print)` | `false` | `true` |

and `tostring(print)` printed `<function: 0x5a3c2cdc3d40>` on one run and
`<function: 0x5f3312f24d40>` on the next run of the same binary.

### #5 (High) - equality and hash compare the payload slot, not the function

`src/vm/lua_val.rs:178-195` (PartialEq) and `:153-176` (Hash):

```rust
(RustFn(a), RustFn(b)) => {
    let x: *const RustFunc = a;   // a binds as &RustFunc via match ergonomics,
    let y: *const RustFunc = b;   // so this is the address of the payload slot
    x == y                        // *inside each Val*, not the fn address
}
```

`a` and `b` bind as `&RustFunc`, so coercing to `*const RustFunc` yields the
address of the storage holding the function pointer, not the function's own
address. Two `Val`s holding the same function always live at different
addresses - `OP_EQUAL` pops both operands into separate locals
(`frame.rs:262-271`) - so equality is effectively always false. `Hash` has the
same bug and additionally hashes a stack address, which is not deterministic
across runs.

Consequences beyond the table above:

- Host functions are broken as table keys. Repeated assignment appends
  duplicate entries, because the inline scan and the IndexMap probe both fail
  and the probe hash differs from the stored hash.
- `format_pointer_id` (`src/vm/table_ops.rs:432-445`) probes with
  `*candidate == val`, never matches for a `RustFn`, and mints a fresh id on
  every `%p` call. That defeats the deterministic-identity feature `%p` exists
  for, and leaks ids at an unbounded rate.
- The method-IC validation `index_handler != entry.index_handler`
  (`eval_index.rs:299`) can never validate a cached `__index = <RustFn>`
  handler, so those callsites permanently miss. Same bug, perf symptom.

The correct comparison already exists elsewhere in the crate:
`std::ptr::fn_addr_eq` is used properly at `eval_control.rs:138-143`.

### #23 (Medium) - `tostring(fn)` leaks an ASLR-dependent address

`src/vm/lua_val.rs:105, 117` (`to_string_with_heap` / `to_bytes_with_heap`)
render a `RustFn` as `<function: 0x...>` using the real function pointer. Under
PIE/ASLR that address differs between runs of the same binary, as measured
above. Scripts can branch on the string (`tostring(print):find("7")`), so this
is replay-visible, not cosmetic - and determinism is a product requirement, not
a style preference.

Tables already solved this: `ObjectPtr`'s Display prints the slotmap key, and
`%p` mints deterministic ids. Lua closures are fine for the same reason.

Note TODO.md currently claims "nothing observable depends on its stability",
which the measurement above contradicts.

### Related inconsistency to resolve at the same time

`Debug`/`Display` for `Val::RustFn` (`lua_val.rs:130, 145`) format
`func: &RustFunc` with `:p`, i.e. the reference's target - the payload slot -
while `to_string_with_heap` takes it by value and prints the actual function
address. So `tostring(print)` and a debug-printed error context show different
"addresses" for the same value.

### #55 (Low) - TODO.md entry is stale

`TODO.md:79-98` ("Stable RustFunc identity for serialization") claims dellingr
ships no serialization story and proposes registration-time stable ids. The
snapshot feature already ships exactly that: `State::register_rust_fn`,
`set_global_named_rust_fn`, dotted stdlib ids, and save-side
`rust_fn_ids_by_addr`. It also describes the current code as hashing "by
function-pointer address", which is wrong in dellingr's favour - the bug is
worse than what TODO tracks. Per the backlog convention this entry should be
deleted or rewritten down to whatever sliver actually remains after this loop.

---

## Agreed implementation plan

All of it lives in `src/vm/lua_val.rs`. **No signature changes anywhere.**

### Equality and hash

- `PartialEq`: `(RustFn(a), RustFn(b)) => std::ptr::fn_addr_eq(*a, *b)`.
- `Hash`: `RustFn(func) => (*func as usize).hash(hasher)`.

This satisfies the `Eq`/`Hash` contract: whenever `fn_addr_eq` is true the
addresses are equal, so the hashes are too. Rust guarantees that for compatible
function-pointer types a true result means calling either pointer is
equivalent, and both sides are exactly `RustFunc` here - so ICF folding stays
safe, consistent with the policy `rust_fn_ids_by_addr` already relies on.

The codegen-unit false negative (one source function independently materialized
as two pointer values) remains theoretically possible. It is not a soundness
problem, ordinary VM copies including repeated reads of `print` preserve the
same pointer bits, and the only stronger fix would be replacing `RustFunc` with
an ID-bearing handle - not warranted.

ASLR makes the *internal* hash differ between processes. That does not violate
observable determinism: table iteration follows `IndexMap` insertion order, the
hash is never exposed to scripts, instruction cost does not depend on bucket
placement, and snapshots serialize registered string ids rather than hashes or
addresses.

### Rendering

Add a private `const RUST_FN_DISPLAY: &str = "<function>";` and render **every**
`Val::RustFn` as exactly that:

- `to_string_with_heap` -> owned `<function>`.
- `to_bytes_with_heap` -> **borrowed** `b"<function>"` (avoids an allocation).
- `Debug` and `Display` -> `f.write_str(RUST_FN_DISPLAY)`.

So `tostring(print)`, `print(print)`, `string.format("%s", print)`, and any
debug-printed error context all agree, which also settles the Debug/Display
inconsistency.

Rejected alternatives and why: threading `&mut State` into conversion is
impossible for `Debug`/`Display`; reusing snapshot registration ids would make
rendering depend on whether the crate was built with the `snapshot` feature,
and unregistered Rust functions are legal without it; pre-assigning ids would
mean widening `Val` or maintaining another registry at every insertion path;
and sharing lazy ids with `%p` would let ordinary string conversion perturb
subsequent `%p` allocation.

This is deliberate: Lua specifies `tostring` only as human-readable and makes
no uniqueness promise, while `%p` is the documented unique string identifier.
Lua closures and tables are unchanged - they already render deterministically
via the slotmap key.

### Deliberately unchanged

- `format_pointer_id` (`table_ops.rs:435`) and the `%p` formatting code. Fixing
  equality fixes them transitively, since the probe is `*candidate == val`.
- `State::to_string`, `to_string_with_meta`, byte coercion, and their
  signatures.
- `register_rust_fn` (`vm.rs:576`), `rust_fn_ids_by_addr`, and save/load
  encoding: they keep using code-address identity internally and stable string
  ids in serialized data.
- The method-IC code (`eval_index.rs:299`). Its validation predicate starts
  working by itself.

### Tests

Unit tests beside `Val` in `src/vm/lua_val.rs`:

- two separately stored `Val::RustFn` copies of one pointer compare equal;
- their hashes match, via a recording `Hasher`;
- behaviourally different callbacks compare unequal;
- `Debug`, `Display`, string conversion and byte conversion all yield
  `<function>`.

New `tests/rustfn_identity.rs`:

- `print == print`, a local alias equals `print`, `rawequal(print, print)`, and
  a different builtin compares unequal;
- Rust functions work as table keys in **both** inline and IndexMap-promoted
  tables;
- reassigning through the same function key updates rather than appending a
  duplicate entry.

Extend `pointer_format_is_deterministic_and_identity_based`
(`tests/string_format.rs:168`) with a fresh-state case: `%p` on `print` twice
gives `0x1|0x1`, and a distinct Rust function then gets `0x2`.

Add to `tests/save_state.rs`: register one function under two ids in opposite
orders in two states, store it as a reachable global, and assert both saves are
byte-identical - pinning the lexicographically-smallest `rust_fn_ids_by_addr`
behaviour.

### Cleanup

Delete #5, #23 and #55 from `notes/bugs.md`, and delete the stale "Stable
`RustFunc` identity for serialization" section from `TODO.md`.
