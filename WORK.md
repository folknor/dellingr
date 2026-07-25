# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #22, #46, #47: host API and stack discipline

Three defects on the host-facing surface. None is reachable from pure Lua -
they need a host `RustFunc`, or they affect only diagnostics - so verification
is Rust-side, not a diff against reference Lua.

### #22 (Medium) - `instr_tfor_call_rust_fn` truncates the result count

`src/vm/eval_control.rs:269`:

```rust
let num_ret_actual = self.get_top() as u8;
```

If a host-provided iterator `RustFunc` leaves more than 255 values on its
frame, the cast wraps (256 -> 0). The `Greater` arm then pushes spurious nils
and the subsequent `rotate_right`/`truncate` bookkeeping leaves hundreds of
stray values inside the loop frame - silent stack corruption instead of a
"too many results" error.

The sibling path in `State::call` (`src/vm/eval.rs:102-117`) does exactly this
comparison correctly in `usize`:

```rust
let num_ret_actual = self.get_top();
let reported = usize::from(num_ret_reported);
match reported.cmp(&num_ret_actual) { ... }
```

Mirror it. Not reachable from stdlib iterators; requires a host RustFunc used
as a generic-for iterator.

### #46 (Low) - stack-trace lines are stale for non-`OP_CALL` invocations

Only `OP_CALL` refreshes `call_info.ip` (`src/vm/frame.rs:215-219`). Calls made
by `OP_TFOR_CALL`, by `__index`/`__newindex`/`__len`/`__tostring` metamethod
dispatch, and by `table.sort` comparators leave the caller's `CallInfo.ip`
pointing at the previous `OP_CALL`. Tracebacks therefore report the wrong
caller line for errors raised inside iterators and metamethods.

This matters more since the previous loop: error messages now render
`chunk:line:` from frame information, so a stale `ip` is user-visible in the
message itself, not only in the traceback body.

Fix by refreshing `ip` at those dispatch sites too, or by deriving it from the
live `Frame` when the trace is built - the latter is one place instead of
several, if it is reachable there.

### #47 (Low) - `set_top` growth bypasses `MAX_STACK_SIZE`, and the API mixes panics with errors

`src/vm/stack.rs:24-56`. Two separate problems in one small API:

1. `set_top(i)` with a large positive `i` pushes nils in a loop with **no
   `check_stack_space`**, so a host - or a buggy `RustFunc` - can request
   `set_top(isize::MAX)` and OOM the process, bypassing the 1M-value cap
   enforced everywhere else.
2. `set_top` and `pop` `assert!` (panic) on misuse, while `insert`, `remove`,
   `replace` and `push_value` return `ErrorKind::InvalidStackIndex`. Given that
   error paths are required to leave the State consistent and reusable, the
   panicking forms are the odd ones out.

`pop_val` panicking is different and should stay: it is documented as "VM bug,
not a user error" and is an internal invariant, not a host-input path.

### Constraints

- Determinism unaffected; charge nothing new (#16).
- Changing `set_top`/`pop` to return `Result` is a **public API break**. The
  crate is pre-1.0 and explicitly unstable, and the previous loop already broke
  `State::to_string`. Still, call it out.
- `unwrap_used` denied outside `#[cfg(test)]`; clippy denies warnings.

---

## Agreed implementation plan

Settled between the orchestrator and the deep reviewer. **Two claims in the
problem statement above were wrong and are corrected here** - read these first.

### Correction 1: #22 is not a "too many results" rejection

`get_top()` is the whole RustFunc frame, **including arguments the callback
never removed**. More than 255 visible values therefore does not mean more than
255 reported results, so introducing a `u8::try_from(get_top())` rejection
would fail correct callbacks.

The fix is to compare in `usize` and retain the topmost reported values,
exactly as `State::call` already does (`eval.rs:102-117`). Keep both the actual
and reported counts as `usize` through the comparison, rotation, truncation and
balancing.

Confirmed unique: `eval_control.rs:269` is the only `get_top() as u8` on a host
result path. Lua's own dynamic return path already uses checked `u8::try_from`
(`eval.rs:422`), `unpack` checks its span before casting (`lua_std.rs:60`), and
pattern captures are capped at 32.

### Correction 2: #46 does not corrupt the rendered message line

A stale `CallInfo.ip` affects **only outer traceback entries**.
`locate_in_frame` takes the position from the live frame's `current_line`
(`eval.rs:339`), `build_stack_trace` puts that live frame first (`vm.rs:801`),
and rendering uses that first frame for the prefix (`error.rs:260`).

Concrete case:

```lua
local function iter()
  error("boom")       -- line 2
end
print("before")       -- line 4, the caller's last OP_CALL
for x in iter do end  -- line 5, OP_TFOR_CALL
```

Prefix and innermost entry are correctly `chunk:2` today. The *caller* entry
wrongly says line 4; it should say 5.

### #46 - refresh `CallInfo.ip` at dispatch sites

Deriving lines at trace-build time is not possible: `build_stack_trace` gets
only the innermost live `Frame` and derives every outer entry from
`CallInfo { bytecode, ip }` (`vm.rs:798-807`), which retains no `Frame`
(`vm.rs:125`); and once an inner error carries a trace, outer frames do not
rebuild it (`eval.rs:399`).

Add a small `Frame` helper that records `self.ip` into the current `CallInfo`,
and call it from every bytecode dispatch site that can re-enter Lua:

- `OP_CALL` (`frame.rs:215`) - keep the existing refresh
- `OP_TFOR_CALL` (`frame.rs:293`)
- `OP_LENGTH` for `__len` (`frame.rs:301`)
- `OP_GET_FIELD` / `OP_GET_TABLE` for `__index` (`frame.rs:307`)
- `OP_SET_FIELD` / `OP_SET_TABLE` for `__newindex` (`frame.rs:385`)

The other direct `State::call` sites - `gsub` replacement functions
(`string.rs:183`), `gmatch` construction (`640`), `table.sort` comparators
(`table_ops.rs:379`), `__tostring` (`422`), anchored host calls
(`anchor.rs:244`), recursive `__call` (`eval.rs:162`) - **need no refresh**:
they are entered through `OP_CALL`, so the caller ip is already right.

### #47 - `set_top` and `pop` return `Result<()>`

Both become fallible. Validate everything **before** mutating, so a rejected
call leaves the stack untouched:

- `set_top`: reject a negative index that lands below the bottom, and call
  `check_stack_space(new_top - old_top)` before growing. Today the growth arm
  pushes nils in a bare loop, so `set_top(isize::MAX)` OOMs the process.
- `pop`: reject counts above the top **and negative counts**, which are
  currently accepted silently because the assertion only checks the upper bound
  and `0..n` is empty for negative `n`.
- `pop_val` stays panicking: it is an internal invariant ("VM bug, not a user
  error"), not a host-input path.

Clamping was rejected: it silently produces a different stack shape than the
host asked for. Keeping the panics and only adding the bound was rejected too -
it fixes the OOM but leaves the inconsistent contract, where siblings
`insert`/`remove`/`replace`/`push_value` all return `InvalidStackIndex`.

**Churn:** 89 production call sites (67 `set_top`, of which 54 are
`set_top(0)`; 22 `pop`), 140 repository-wide. Nearly all are already inside
`Result`-returning RustFuncs, so it is mechanical `?` propagation. Nothing
depends on oversized growth: every production positive literal is 1..3 and the
tests only exercise zero and negative forms (`tests/rustfn_error.rs:53`).

**Public API break** - call it out in the commit message.

**Out of scope, record only:** `check_stack_space` is *not* enforced
everywhere. `push_nil`, `push_number` and the other public push methods append
without checking (`stack.rs:65`); it is mainly applied when preparing Lua
frames (`eval.rs:363`). Fixing `set_top` closes the bulk-allocation hazard; a
host-wide stack-cap audit is separate work and gets its own finding.

### Tests

- Host iterator leaving 256 visible values while reporting one topmost result:
  one loop iteration completes and the State stays clean and reusable.
- `set_top(isize::MAX)`, a negative index below the bottom, an excessive `pop`,
  a negative `pop` - each returning an error with the stack unchanged - plus
  the normal negative `set_top` semantics still working.
- Line-sensitive traceback tests for a failing generic-for iterator and for
  `__index`, `__newindex` and `__len`, asserting **both** the innermost message
  line and the outer caller line. Add `table.sort` and `__tostring` cases as
  regression coverage even though those already inherit a correct `OP_CALL`
  line.

Charge nothing new.

Read `src/vm/eval_control.rs` (`instr_tfor_call_rust_fn`), `src/vm/eval.rs`
(`State::call`'s RustFn arm), `src/vm/stack.rs`, `src/vm/frame.rs`, and
`src/vm.rs` (`build_stack_trace`).
