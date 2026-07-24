# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #1, #2, #4, #25: values held outside the GC root set

Four instances of one defect class. `vm::mark_gc_roots` (`src/vm.rs:69-89`) is
documented as the single source of truth for reachability, and marks exactly:
`stack`, `globals`, `builtins`, `string_literals`, `active_call_roots`,
`upvalue_pool` (transitively), and `registry`. Each finding below holds a live
`Val` somewhere that set cannot see, across an operation that can trigger
`gc_collect`. The result is a swept-but-referenced object and a
`"Invalid ObjectPtr: object was freed (use-after-free detected)"` panic - which
kills the host process, violating the design rule that errors kill the callback
rather than the host.

They are one loop because they want one mechanism, and because `mark_gc_roots`
should be edited once rather than three times.

All four verified by reading.

### #1 (High) - `with_restricted_env` un-roots the saved environment

`src/vm.rs:611-654`. The function `std::mem::replace`s the real environment
into Rust locals:

```rust
let saved_globals = std::mem::replace(&mut self.globals, restricted_globals);
let saved_builtins = std::mem::replace(&mut self.builtins, restricted_builtins);
```

While `f` runs, those locals are invisible to `mark_gc_roots`. Any allocation
inside `f` can collect everything reachable only from the saved environment:
the `math` / `string` / `table` library tables, the `_G` proxy and its
metatable, and every non-whitelisted user global holding an object. After
restore, `state.globals` / `state.builtins` hold dangling `ObjectPtr`s.

The existing panic guard (`catch_unwind`, line 643) restores the environment on
unwind, but restoring dangling pointers does not help: the "restored after the
function completes (or errors)" guarantee is hollow whenever a GC ran inside
`f`. There is no restricted-env-plus-GC coverage in `src/vm/tests.rs`.

Note this must nest - `with_restricted_env` can be called reentrantly.

### #2 (High) - frame varargs are not roots

`src/vm/eval.rs:309-316`:

```rust
let varargs = if is_vararg && num_args > num_params {
    let num_varargs = (num_args - num_params) as usize;
    let vararg_start = self.stack.len() - num_varargs;
    self.stack.drain(vararg_start..).collect()
} else {
    Vec::new()
};
```

The values are drained *off* the VM stack into a plain `Vec<Val>` that is moved
into the `Frame` (`src/vm/frame.rs:29`), which is a Rust local, not part of
`State`. Any allocation inside the frame - a table constructor, a concat, a
closure, or the per-call literal interning in `initialize_frame` itself - can
collect a value reachable only through `frame.varargs`. The later `OP_VARARG`
(`frame.rs:229-247`) pushes the stale pointer.

Existing tests likely miss this because benches pass numeric varargs, and
`Val::Num` is not heap-managed.

Repro:

```lua
local function f(...)
  local junk
  for i = 1, 200 do junk = {i} end
  return ...
end
print(f({ "boom" }))
```

### #4 (High) - `#t` with `__len` drops the receiver across `alloc_string`

`src/vm/eval_store.rs:139-184`. The operand is popped at line 141
(`let val = self.pop_val()`), and the metatable path calls
`self.alloc_string("__len")` at line 162. `alloc_string` (`vm.rs:656-665`) runs
`gc_collect` whenever `heap.is_full()`. Between the pop and lines 170-171, where
`len_handler` and `val` are pushed, neither the table nor its metatable is
rooted - the metatable is reachable only through the table. If the operand was
a temporary, both are collected and `self.heap.as_table_ref(mt_ptr)` panics; if
the metatable survives, the `self.stack.push(val)` at 171 reinstates a dangling
pointer that the `__len` body then dereferences.

The sibling paths already get this right: `src/vm/metamethod.rs:49-52, 170-173`
push key/val around `alloc_string` precisely to protect them, and the `__call`
path parks `func_val` in `active_call_roots`. This is the smallest of the four
and may just be that same push/pop pattern.

### #25 (Medium) - `table.sort` runs the comparator with the array detached

`src/vm/table_ops.rs:280-330`. `t.get_array()` at line 288 copies the array
portion into a local `Vec<Val>`, and the comparator branch then calls arbitrary
Lua code at line 321 in a loop while `arr` is reachable only from Rust. A
comparator that clears the table and allocates enough to trigger GC gets the
not-currently-passed elements collected; the next iteration pushes a dangling
`Val` as a comparator argument, and the eventual `set_array` writes dangling
pointers back into the table for a deferred panic.

Repro:

```lua
local t = {}
for i = 1, 20 do t[i] = { v = i } end
table.sort(t, function(a, b)
  for k in pairs(t) do t[k] = nil end
  for i = 1, 200 do local _ = {} end
  return a.v < b.v
end)
```

### Reproduction status

#2, #4 and #25 were each run against the debug binary and each aborted the
process with `Invalid ObjectPtr: object was freed (use-after-free detected)` at
`src/vm/object.rs:224`. #1 is host-API-only and was confirmed by reading.

Nothing protects #1 indirectly: `env_tokens` is a weak classification map, not
a marked root; inline caches are not roots; the registry covers only explicitly
anchored values; and restoring the saved maps after a collection just restores
invalid generational pointers.

---

## Agreed implementation plan

One mechanism for all four. Generalize `active_call_roots` into a State-owned
transient-root registry. **Do not** park these values on the visible VM stack -
that would perturb `stack_bottom`-relative indexing and `get_top`.

### Structures

```rust
struct TransientRoots {
    values: Vec<Val>,                          // active closures + scoped temporaries
    suspended_envs: Vec<SuspendedEnvironment>,
}

struct SuspendedEnvironment {
    globals: IndexMap<String, Val>,
    builtins: [Val; Builtin::COUNT],
}
```

`State::active_call_roots` becomes `State::transient_roots`; the existing
active-call push/pop sites (`src/vm/eval.rs:121, 124, 143, 150`) move to
`transient_roots.values` unchanged. Suspended environments need their own shape
only so the maps can be moved out and restored without flattening every global
into a vector.

Implement `Markable` for both. `mark_gc_roots` then marks: stack, current
globals, current builtins, string literals, the whole transient-root registry,
and the registry anchors. The upvalue pool stays a parameter for transitive
closure marking and does not become an independent root.

Note `mark_gc_roots` has exactly one caller (`vm.rs:461`) and already carries a
`too_many_arguments` allow; folding its parameters into `&State` while adding
the new root set is in scope and makes "single source of truth" structural
rather than aspirational.

### Scoped helpers

Add internal `with_rooted_value` / `with_rooted_values`:

1. record `transient_roots.values.len()` as a watermark,
2. append the root copies,
3. run the operation,
4. truncate back to the watermark before returning,
5. where Rust unwinding is in play, catch, truncate, `resume_unwind`.

Watermarks make nesting naturally LIFO, so a comparator may recursively sort
another table or call a vararg function without disturbing outer roots.

### Per-finding application

- **#1**: move the displaced maps into `transient_roots.suspended_envs`
  immediately after the `mem::replace`, and restore by popping. Nesting works
  because the inner call restores `suspended[1]` and the outer later restores
  `suspended[0]`. Must restore on normal return, on `Err`, and on unwind.
- **#2**: leave `Frame::varargs` as it is; additionally copy the extra varargs
  into `transient_roots.values` for the frame's lifetime. **The root operation
  must live strictly inside the existing `is_vararg && num_args > num_params`
  branch** - fixed-arity calls and vararg calls with no extras must gain no
  push, no allocation, no watermark bookkeeping, and no cleanup branch.
- **#4**: root the popped receiver around the `"__len"` allocation and the
  metatable lookup. The root must be released before cost flushing and the
  metamethod call, by which point the receiver is back on the VM stack.
- **#25**: copy `arr` into `transient_roots.values` around comparator
  execution and keep sorting the local `Vec`. Swapping within `arr` does not
  change the rooted *set*, so one copy up front is sufficient. The extra copy
  is acceptable in a function that is already an O(n^2) bubble sort.

Also harden `set_table_str_key_value` (`src/vm/table_ops.rs:38`, recorded as
C-E1) with `with_rooted_value` in the same patch. It is genuinely latent today
- every caller passes `RustFn` or `Num` via `set_table_str_key_rust_fn`,
`set_table_str_key_named_rust_fn`, or `set_table_str_key_number` - but the
signature is a standing trap.

Do not wait for the interpreter-flattening rewrite (`optimizations.md` #2).
This is a live host-panic defect, and the targeted roots are compatible with
that rewrite: once frames are State-owned, the extra vararg roots just get
deleted.

### Error and unwind paths

Every one of these must release its roots, or `validate_quiescent` will start
failing after killed callbacks:

- vararg roots: released after the existing frame stack / literal / call-info
  cleanup,
- sort roots: released before propagating a comparator error,
- the `__len` root: released before cost flushing and metamethod invocation,
- restricted environments: restored even when `f` returns `Err`.

Update `validate_quiescent` (`src/vm/save_state.rs:464`) to require
`transient_roots.is_empty()`, and clear it during snapshot materialization
alongside the other transient state (`save_state.rs:591`). That makes a leaked
root visible instead of silent.

### Tests

These must force collection explicitly - via `gc_collect()` or by arming the
threshold to the current heap size. Allocation churn alone is too indirect to
pin these regressions.

In `src/vm/tests.rs`:

- `gc_preserves_nested_suspended_environments` - original global object, enter
  an outer restriction creating an outer-only global object, enter a nested
  restriction, `gc_collect()`, then verify the outer-only object after inner
  restoration and the original object plus `math` after outer restoration.
- `gc_preserves_frame_varargs` - a Rust `force_gc` builtin, then
  `local function f(...) force_gc(); return ... end; return f({marker=42}).marker`.
  The table must be a temporary passed directly, not a global or a caller local.
- `length_receiver_survives_lookup_collection` - an `arm_gc` builtin setting
  the threshold to `heap_size()`, a helper returning a temporary table, `#`
  applied to that temporary. Assert the `__len` result *and* `!gc_should_run()`
  afterwards, proving the armed allocation actually collected.
- `table_sort_array_survives_comparator_collection` - object elements; the
  comparator nils every entry and calls `force_gc()` on first invocation.
- `set_table_str_key_value_roots_heap_value` - destination table on the stack,
  a fresh source table held only in the Rust `val` local, arm the next string
  allocation, call the helper, read the child back.

Strengthen `restricted_env_restored_after_panic`
(`tests/error_handling.rs:826`) by forcing `gc_collect()` immediately before
the deliberate panic and then verifying an object-valued non-whitelisted global
after catching it.

In `tests/save_state.rs`, quiescence after each unwind path:

- error from a vararg frame with extra object arguments, then `save_state()`,
- error from a `table.sort` comparator, then `save_state()`,
- error returned from inside `with_restricted_env`, then `save_state()`,
- `save_state()` while an environment is suspended must return `NotQuiescent`.
