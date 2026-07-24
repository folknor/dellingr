# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #3 and #29: `Table::next` key handling

Both live in the same function and share a fix surface, so they are one loop.

### #3 (High) - `next(t, 0/0)` panics the process

`src/vm/table.rs:512-548`. `Table::next` has no NaN guard. On `TableStorage::Map`
it calls `map.get_index_of(key)` (line 539), which hashes the key, and
`Hash for Val` (`src/vm/lua_val.rs:162`) is:

```rust
assert!(!n.is_nan(), "Cannot use NaN as table key");
```

A hard `assert!`, so it fires in release too. `Table::get` / `get_with_index` /
`insert` all guard NaN at entry (`table.rs:122, 152, 298`); `next` was missed.

Inline storage (<= 4 entries) does not panic: the linear scan at
`table.rs:522-529` compares with `PartialEq`, NaN compares unequal to
everything, the loop falls through and returns `(nil, nil)`. So the panic is
storage-dependent - the table must have been promoted to Map.

Reference Lua raises `invalid key to 'next'`.

Verified repro (unrun):

```lua
local t = {}
for i = 1, 10 do t[i] = i end   -- force Map storage
next(t, 0/0)                    -- dellingr: panic; reference: error
```

### #29 (Medium) - missing key silently ends iteration

Same function. Both storage arms fall through to `(Val::Nil, Val::Nil)` when
the key is not found, which is indistinguishable from end-of-iteration.
Reference raises `invalid key to 'next'`. This is the same defect class as #3
(unvalidated control key) and masks #6 (removing the current key during
`pairs` ends the loop early), which is explicitly out of scope for this loop.

### Call sites that must carry the new error

- `src/vm/table_ops.rs:155-176` (`State::table_next`) - returns `Result<bool>`,
  already has an error channel (`self.type_error(...)` at line 174).
- `src/lua_std/basic.rs:77` - the `next` builtin, goes through `table_next`.
- `src/vm/eval_control.rs:186-207` (`instr_tfor_call_next`) - **returns plain
  `bool`, no error channel.** `true` means "handled", `false` means "fall back
  to the generic path". Raising a Lua error from the `pairs` fast path needs a
  signature change or a fallback-to-slow-path signal. This is the one real
  design question in the loop.

### Constraint

`Table::next` is `#[hotpath::measure]`d and sits under `pairs`, a hot path. The
guard must not cost a hash or an allocation on the common path.

### Scope note

Fixing #29 turns #6 (removing the current key during `pairs`) from premature
termination into an `invalid key to 'next'` error. It does not make
deletion-during-iteration conformant. #6 stays open and out of scope here.

---

## Agreed implementation plan

### 1. `src/vm/table.rs`

Add a crate-private three-state result and change `Table::next` to return it:

```rust
pub(super) enum TableNext {
    Pair(Val, Val),
    End,
    InvalidKey,
}

pub(super) fn next(&self, key: &Val) -> TableNext
```

Semantics, both storage arms:

- `key` is nil, table empty -> `End`.
- `key` is nil, table non-empty -> `Pair(first)`.
- `key` found with a successor -> `Pair(successor)`.
- `key` found and it is the **last** entry -> `End`.
- `key` not found -> `InvalidKey`.
- `key` is `Val::Num(NaN)` -> `InvalidKey`, **rejected before any hashing**.

**The easy thing to get wrong:** the current inline arm `break`s out of the
scan when it finds the key at the last position, and falls through to the same
`(Nil, Nil)` as the not-found case. Those two cases must now return different
variants (`End` vs `InvalidKey`), so the loop needs restructuring rather than a
mechanical return-value swap. Same distinction in the Map arm: `get_index_of`
returning `None` is `InvalidKey`, whereas a found index with no `index + 1` is
`End`.

The NaN guard is a single `is_nan()` on an already-matched `Val::Num`, mirroring
what `Table::get` already does at `table.rs:122`. No hash, no allocation.

Update the doc comment, which currently documents the `(nil, nil)` contract.

### 2. `src/vm/table_ops.rs` (`State::table_next`)

Signature stays `Result<bool>`. Match on `TableNext`:

- `Pair(k, v)` -> push both, `Ok(true)`.
- `End` -> push nil, `Ok(false)`.
- `InvalidKey` -> `Err(ErrorKind::RuntimeError("invalid key to 'next'".into()))`.

`base_next` (`src/lua_std/basic.rs:77`) needs no change; it propagates with `?`.

### 3. `src/vm/eval_control.rs` (`instr_tfor_call_next`)

Signature stays `bool` - **do not** change it to `Result<bool>`.

- `Pair` / `End` -> write results as today, return `true`.
- `InvalidKey` -> make **no** stack or slot changes and return `false`.

`false` already means "the fast path declined". `instr_tfor_call`
(`eval_control.rs:128-148`) then falls through to `instr_tfor_call_rust_fn`,
which invokes `base_next`, which calls `State::table_next`, which raises the
error. So the error surfaces through the existing channel with zero
`Result`-propagation cost on successful iterations, and the duplicated lookup
happens only on the failing path.

`instr_tfor_call` itself is unchanged.

### 4. `src/error.rs`

No change. Reuse `ErrorKind::RuntimeError`. Not `TypeError::TableKeyNan`: its
message is "table index was NaN", and this one error must also cover non-NaN
missing keys. Reference Lua's message is exactly `invalid key to 'next'`.

### 5. Tests

Unit tests in `src/vm/table.rs`'s existing test module:

- `next_distinguishes_end_from_invalid_key_inline`
- `next_distinguishes_end_from_invalid_key_map`

Each covers: initial nil, a valid middle key, a valid final key (must be `End`),
an absent key (must be `InvalidKey`), and NaN (must be `InvalidKey`). Force map
storage with at least five entries.

Integration tests in `tests/error_handling.rs`:

- Direct `next(t, k)` rejects absent and NaN controls, inline and map-backed.
- `for ... in next, t, <bad key> do end` raises the same error, exercising the
  `instr_tfor_call_next` fallback path.
- Assert `ErrorKind::RuntimeError` with the exact message `invalid key to 'next'`.

Do **not** add a test asserting that a *removed* current key is invalid - that
would encode #6's divergence as intended behavior.

Nothing goes in `examples/`: after the fix these raise errors, and a nonzero
exit fails `tests/run_examples.rs`.
