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

# Runtime defects: dispatch, VM core, tables

Filed from the 2026-10-23 defect hunt at a22d9c0 (scopes `dispatch`,
`vm-core`, `tables`, plus lateral notes from `codegen` and `strings`). The
hunters read the code statically and built or ran nothing, so every repro here
is traced by hand, not executed. The `tables` hunter asks that each repro become
a test before anyone acts on it.

## RT-002 - "Its bytecode never checks it again" is false: register ops still fail StackOverflow mid-body

From the `dispatch` hunter, rated low to medium: a contract statement is wrong,
and the behaviour is inconsistent.

**Claims broken.** `admit_lua_call` says:

> The frame reserves every register it can use, parameters, locals and
> temporaries, against the stack cap now, so its bytecode never checks it
> again.

docs/limits.md frames the cap the same way: a call "fails there with
`StackOverflow`, before its body runs".

**Mechanism.** Several plain register instructions still grow the stack above
the frame top and check the cap. None of them is a metamethod path:

- `OP_CLOSURE`: `reg_closure` -> `instr_closure` -> `push_closure`, which does
  `check_stack_space(1)`, then pushes and pops into `dst`.
- `OP_NEW_TABLE_TEMPLATE`: `new_table_with_template` does
  `check_stack_space(1)`. `OP_NEW_TABLE` and `OP_NEW_TABLE_PRESIZED` allocate
  straight into the register and never check. So at the cap `{}` works and
  `{x = 1}` raises `StackOverflow`.
- `OP_CONCAT`: `reg_concat` does `check_stack_space(count)` and copies every
  operand above the frame, then concatenates there. So `a .. b .. c` needs 3
  free slots above a frame that already owns `a`, `b` and `c`.
- Every slow path needs scratch: `get_field_slow` 1, `get_table_slow` 2,
  `set_field_slow` 2, `set_table_slow` 3, `len_slow` 1, plus 1 for an
  `__index` result and 3 or 4 more for a handler call (an `__index` chain of
  tables no longer needs a slot per table). Which reads take a slow path no
  longer depends on cache state, but it still depends on spelling: at the cap,
  `obj.k` through a one-level `__index` answers in its register, while
  `obj["k"]` (no direct `__index` route in `reg_get_table`) and every string
  receiver take a slow path and can raise `StackOverflow`.
  `tests/index_depth_routes.rs` pins only that cache history does not matter,
  so this rewrite may change those outcomes. Writes split the same way:
  `t.newkey = v` on a plain table always takes `set_field_slow` (2 scratch
  slots), because the direct store route only updates existing keys, while
  `t[k] = v` inserts directly, so at the cap only the first spelling raises.
- `index_string`'s cache-hit path (`string_index_handler`) does
  `check_stack_space(1)` and a push, which the register path then pops.
  notes/optimizations.md already tracks "string method calls always take the
  stack slow path"; this is the same scratch dependency.

A frame whose top sits within a few slots of `MAX_STACK_SIZE` is admitted, then
dies partway through its body on a closure, a template constructor or a
concatenation. Where it dies depends on which of these it reaches first.

**Side note on `OP_CLOSURE`.** `instr_closure` calls `find_or_create_upvalue`,
which inserts into `open_upvalues` and allocates pool slots, before
`push_closure`'s preflight refuses. The rejected instruction is therefore not
side-effect free. It is harmless only because the error always kills the frame,
and unwinding closes those upvalues.

**Deferred (user, 2026-10-23),** together with RT-014 and the register-native
slow-path rewrite.

**Direction.** Make the claim true rather than reword it:

- allocate the closure and the template table directly into `dst`, as
  `alloc_table` already does;
- concatenate from the register range in place (`concat_helper` only needs a
  slice);
- reserve a fixed handler headroom at admission, the largest call window a slow
  path opens (4), so that only open-results windows and Rust callbacks can push
  past a frame's reservation.

Admission then fails up front, as limits.md describes. If the headroom is not
wanted, fix the comment and limits.md to list the instructions that still check.

## RT-014 - Every slow-path entry repeats the fast-path lookup

From the `dispatch` hunter (its L1) and the `tables` hunter (its L2). Raised as
performance observations outside the defect bar.

Deferred (user, 2026-10-23): these go with the register-native rewrite, with
RT-002.

- `get_field_slow` -> `instr_get_field` re-runs `get_table_field_direct` and
  `get_index_table_field_direct`, both of which just failed in
  `get_field_direct`.
- `set_field_slow` -> `instr_set_field` re-runs `try_set_field_cached` and
  `try_set_field_direct`.
- Every `__index` function or two-level chain read therefore pays the lookups
  twice, plus the push and pop of a copy of an already-rooted register. This is
  the same layering as RT-002, and the register-native rewrite proposed there
  removes it.
- Since the method cache stopped caching misses (a miss could not be proved
  exactly as cheaply as the lookup itself), a two-level inheritance site pays
  the direct attempt in full, twice per read. Measured 2026-10-23 on
  plantasjen against 4a722cf, `bench/method_chain.lua` (a monomorphic `s:inc()`
  inherited two levels up) is 14% slower, warm medians 75.8 ms vs 86.5 ms; an
  attribution build that skipped only the repeated attempt saved about 8 ms.
  Removing the double lookup is the recovery; do not bring back an
  unvalidated miss entry.

## RT-015 - A terminal `__newindex` table step still takes a metamethod-depth unit

A residual of the RT-001 resolution (2026-10-23). The read-side rule is now
"a terminal step is free": an `__index` table that answers from its own
contents takes no depth unit, and only steps that continue the chain or call a
handler hold one. The write side was not covered by that ruling and still
charges a unit for every `__newindex` table step, terminal or not. So at the
depth limit `obj.k` through a one-level `__index` table succeeds while
`obj.k = v` through a one-level `__newindex` table raises
`MetamethodDepthExceeded`.

**Direction.** Decide whether the write side follows the same rule. If it
does, apply it on every store route (`.k`, `["k"]`, host `set_table`) and pin it
with limit-edge tests like `tests/index_depth_routes.rs`; if it does not,
docs/limits.md's `MAX_METAMETHOD_DEPTH` wording should say why reads and writes
differ.
