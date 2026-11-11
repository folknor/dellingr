# Defect hunt findings

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

Coverage: the `frontend`, `dispatch`, `heap`, `snapshot`, `host`, `stdlib`,
`cost` and `determinism` scopes were each read by one codex hunter
(gpt-6.1-sol, high effort). `cost` and `determinism` reported no confirmed
defects. The `codegen` scope (`src/compiler.rs`, `src/compiler/codegen.rs`,
`src/compiler/verify.rs`, `src/instr.rs`, `src/disassemble.rs`) was not hunted:
its run was stopped by the provider's safety filter and it produced no report.
Its one surviving progress note, explicitly unconfirmed by the hunter, was: the
verifier checks register bounds but does not establish that stack-shrinking
instructions preserve slots held by open upvalues, so edited snapshot bytecode
might bypass the compiler's convention of keeping call and open-vararg windows
above live locals. The hunter was tracing a minimal case through return cleanup
when the run stopped.

## BUG-001 - Invalid string escapes become falsely recoverable at EOF

Hunter label: P2. Scope: `frontend`. Site: `src/compiler/lexer.rs`, the string
scanner's EOF branches (reported at line 466).

Compiling `return "\q` with EOF immediately after `q` returns
`SyntaxError::UnfinishedString`, making `is_recoverable()` return `true`. The
same happens with unfinished strings containing `\256` or `\xG`.

The lexer has already recorded a permanent escape error, but both EOF branches
discard it. Appending text cannot repair those escapes. Adding the closing quote
merely exposes the original error.

This breaks the `SyntaxError::is_recoverable` contract (`src/error.rs`, reported
at line 1642): appending text can complete the construct. It also fails
`docs/compatibility.md`'s narrower promise about completing the last token,
independently of preceding grammar. Hosts using this API are incorrectly told to
request more input.

Hunter's suggested direction: preserve permanent escape failures at EOF while
keeping incomplete valid prefixes such as `\x4` recoverable.

## BUG-002 - An interrupt refusal consumes host table operands before the Rust handler runs

Hunter label: P2, from both `dispatch` and `host`, reported independently. Sites:
`begin_call` (`src/vm/eval.rs`, reported at lines 449-450), the entry poll in
`invoke_rust_fn` (`src/vm/eval.rs`, reported at line 662), the `get_table`
cleanup (`src/vm/table_ops.rs`, reported at line 324), and the `set_table`
cleanup (`src/vm/table_ops.rs`, reported at line 508).

The broken claim is in `docs/limits.md`, under "Host table methods": `get_table`
and `set_table` preserve their operands when metamethod dispatch fails before
handler entry. `set_table`'s API documentation explicitly includes a handler
call refused before entry.

`begin_call` sets the entry observer before calling `invoke_rust_fn`. That
function can immediately return `Interrupted` from its entry poll, without
invoking the handler. The cleanup nevertheless sees "entered": the `get_table`
cleanup discards the key, and the `set_table` cleanup discards the key and value.

`dispatch` trigger: a host holding the table returned by:

```lua
return setmetatable({}, {__index = tostring, __newindex = rawset})
```

With that table alone at stack index 1:

- Push `"missing"`, trigger the interrupt, then call `get_table(1)`. It returns
  `Interrupted` with stack height 1, although the promised height is 2.
- Independently, push `"missing"` and `42`, trigger the interrupt, then call
  `set_table(1)`. It returns `Interrupted` with height 1, although the promised
  height is 3. No write occurs.

`host` gave the same reproducer with
`setmetatable({}, {__index = print, __newindex = print})`, and noted that neither
handler executes.

This prevents the host from resetting the interrupt and retrying with the
preserved operands. Lua handlers preserve them because their observer is updated
after admission succeeds.

Hunters' suggested direction: commit the entry observer at the Rust invocation
boundary, after its interrupt poll and immediately before the Rust body runs.

`dispatch` states this is established by tracing the entry and cleanup paths, and
that the existing default-sweep runtime, host table atomicity, host set-table and
direct Rust-entry interrupt tests pass without covering this combination. `host`
states its findings are source-traced, not runtime-tested.

## BUG-003 - Upvalue allocation silently wraps and aliases an existing capture

Hunter label: P2. Scope: `heap`. Sites: `UpvaluePool::alloc` (`src/vm/object.rs`,
reported at line 104) and sweep (`src/vm/object.rs`, reported at line 134).

`UpvaluePool::alloc` claims to allocate an upvalue and return its reference, but
narrows `self.slots.len()` from `usize` to `u32` without checking. On a 64-bit
host, allocating slot `2^32` stores the new upvalue there and returns
`UpvalueRef(0)`.

This is reachable by retaining closures with distinct captures. For example,
33,554,432 closures capturing 128 fresh locals each fill the representable range
without exceeding the per-function capture limit.

The next capture aliases slot zero. Closing that capture overwrites an older
closure's variable, and subsequent reads return the wrong value. GC compounds the
corruption: sweep also narrows indices, so sweeping the unreferenced high slot
adds an occupied low slot to the free list.

Hunter's suggested direction: use indices wide enough for the backing vectors, or
reject exhaustion before mutation and propagate an ordinary error. The hunter
states this is established by source tracing and that it did not exercise the
enormous allocation required.

## BUG-004 - `RawField` promises integer implementations it does not provide

Hunter label: P3, from both `heap` and `host`, reported independently. Site:
`src/vm/raw_field.rs` (reported at lines 47-49).

Its public documentation says it supports "every fixed-width integer type."
Implementations cover only signed and unsigned 8-, 16-, 32- and 64-bit integers;
`i128` and `u128` are absent. Consequently, `state.raw_field::<i128>(1, "value")`
fails to compile even for a value such as `1`, as do the `u128` and `Option`
forms. The sealed trait prevents callers from supplying the missing conversion.

Hunters' suggested directions: `heap` says either enumerate the supported types in
the documentation or fulfill the existing promise. `host` says listing the
supported widths explicitly suffices and adding types is unnecessary to resolve
the documentation defect.

## BUG-005 - Load loses rendered identities of literals in nested functions without live closures

Hunter label: P1. Scope: `snapshot`. Sites: materialization
(`src/vm/save_state.rs`, reported at line 1709), capture (reported at line 856),
and the final collection (reported at line 1875).

Materialization creates runtime bundles only for saved closure objects. Nested
bytecode is rebuilt, but its previously populated runtime is omitted.

Reproducer: execute this chunk, return to the host, then save/load with no setup:

```lua
function make()
    return function() return "nested-only" end
end
before = string.format("%p", make()())
```

The continuous State retains the nested function's runtime through `make`'s
bytecode tree. Capture therefore records the literal's rendered identity. During
load, however, the temporary nested closure has no saved object, so its runtime is
never reconstructed. The final collection frees the otherwise unrooted literal and
prunes its identity.

Consequently, `string.format("%p", make()())` produces a new identity after load
while the continuous State retains `before`. An immediate resave also loses the
identity record. This breaks the `docs/save-state.md` contract for rendered
identities and continued byte stability. The hunter says restoring these
identities must also restore their lifetime through reachable nested bytecode.

The hunter states this is source-traced, and that the existing snapshot
integration suite passed, including the current released-format archive check.

## BUG-006 - Gmatch validation rejects legitimate ordinary closures

Hunter label: P2. Scope: `snapshot`. Site: iterator detection
(`src/vm/save_state/verify.rs`, reported at line 742).

Iterator detection assumes that any closure capturing `string.gmatch.iter` or
`string.gmatch.empty_iter` is a built-in iterator wrapper.

The public `registered_rust_fn` API exposes those callbacks. Push either handle as
global `seed`, then execute:

```lua
local f = seed
keep = function() return f end
```

Saving succeeds. Loading rejects the ordinary `keep` closure with
`GmatchCorruption::StateTableMissing`, before setup runs. The same failure occurs
in a raw State that legitimately registers a host function under either id.

This breaks the documented persistence of closures and registered Rust functions,
including the explicit permission for raw States to register stock ids. A captured
registration id alone cannot establish that a closure owns the gmatch state
schema. The hunter states this is source-traced.

## BUG-007 - A compilation error propagated through a Rust callback displays the caller's location instead of the rejected source's

Hunter label: P2. Scope: `host`. Sites: `locate_error` (`src/vm/eval.rs`, reported
at line 1161) and `Error::fmt` (`src/error.rs`, reported at line 1667).

This breaks the documented syntax-error location contract in
`docs/compatibility.md` and produces a misleading diagnostic.

Reproducer: register a Rust callback that returns the error from
`Program::compile("local =", Some(b"child.lua"))`, then invoke it on line 3 of
`outer.lua`. The error retains `child.lua`, line 1 and its column through the
structured accessors, but its displayed prefix becomes `outer.lua:3:`. The rejected
source's location disappears from the diagnostic.

`locate_error` preserves the original location and adds a Lua traceback.
`Error::fmt` then unconditionally prefers that traceback's first frame for ordinary
errors.

Hunter's suggested direction: explicit primary diagnostic locations should survive
traceback attachment; `ScriptError`'s deliberate frame-selection policy can remain
separate. The hunter states this is source-traced, not runtime-tested.

## BUG-008 - The `ipairs` iterator accepts fractional indices and visits fractional keys

Scope: `stdlib`. Sites: `src/lua_std/basic.rs` (reported at line 93) and the
generic-for fast path (`src/vm/eval_control.rs`, reported at line 209).

The library callback checks only that the control argument is a number, then adds
`1.0`. The generic-for fast path repeats this behavior.

```lua
local iter = ipairs({})
print(iter({[2.5] = "fractional"}, 1.5))
-- Prints: 2.5    fractional
```

This breaks the exact-integer requirement for table positions in
`docs/compatibility.md`, and the integer-indexed traversal implied by `ipairs`.
Invalid controls silently select a different sequence instead of raising an
argument error. NaN, infinity, and numbers outside the signed 64-bit range also
pass validation.

The hunter says both paths need the same integer validation, because fixing only
the library callback leaves generic-for behavior wrong. The hunter reproduced this
through stdin.

## BUG-009 - Exhausted `gmatch` iterators return one nil value instead of no results

Scope: `stdlib`. Sites: the no-match exits in `src/lua_std/string.rs` (reported at
line 1467), including the empty-pattern iterator (reported at line 1284).

The no-match exits explicitly push nil and return one result.

```lua
local iter = string.gmatch("abc", "a", 5)
print(select("#", iter()))
-- Prints: 1
-- Expected: 0
```

`src/lua_std.rs` promises that `gmatch` yields nothing when `init` exceeds
`len + 1`. This also affects ordinary exhaustion. Generic-for hides the difference,
but `select`, `table.pack`, and host calls requesting all results observe a phantom
result. Collectors using result count to detect exhaustion consequently fail.

The hunter says every no-match exit should return zero results. The hunter
reproduced this through stdin, and reports another 5,000 pattern cases agreed with
Lua 5.4 after accounting for this issue.
