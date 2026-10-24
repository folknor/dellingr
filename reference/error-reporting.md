# Error reporting and function names

Errors carry a structured `ErrorKind`, source location, and an optional stack
trace. Their `Display` text is for diagnostics. It is not a stable identifier
for an error or a cross-build log format.

The built-in `string.gmatch` wrapper and iterator shim are omitted from runtime
error traces when a script caller exists: lazy pattern errors use the script
caller's call-site line. Recognition uses the shared bytecode identities for
the outer wrapper and its iterator, so a user chunk named `string.gmatch`
retains ordinary attribution. Snapshot load gives a restored iterator the
shared identity: a chunk qualifies only when a closure of it captures the
private gmatch iterator function and its contents equal this build's shim. An
iterator saved by a build whose shim differs keeps its own bytecode and the
shim location.

## Kinds and exact bytes

No kind carries free-form text built by dellingr. Each failure source has its
own typed kind: `ScriptError` (the `ErrorValue` a script passed to `error()`),
`LibraryError` (a `LibraryError` reason), `PatternError`, `SyntaxError`,
`ArgError`, `TypeError` and the limit kinds. `InternalError` wraps an opaque
`InternalError` reason that only dellingr constructs: it always means a
dellingr bug. `HostError` is the one kind dellingr never constructs; it boxes
a `RustFunc`'s own `std::error::Error`, which `Error::source` returns and
`Error::into_kind` hands back owned.

Bytes a script controls reach the host exactly: an `ErrorValue::String` is the
script's string, a table or function error value carries its `tostring`
rendering as bytes, the `found` field of a `SyntaxError` is the offending
token's source bytes, `string.format` directives in `LibraryError` are the
format string's bytes, and chunk names (`Error::source_name`,
`StackFrame::source`) are the bytes given to `Program::compile`. Only
`Display` converts, rendering each invalid UTF-8 sequence as U+FFFD.

The same holds for save and load (`dellingr::snapshot`): every reason inside
a `SaveError` or `SnapshotError` is an enum (`CorruptionReason`,
`DecodeReason`, `InvalidBytecodeReason` with the verifier's
`ValidationReason`, `TableRestoreStep` and `TableRestoreFailure`,
`SaveCollection`, `SnapshotField`), and `SaveError::Internal` wraps an opaque
`SaveInternalError`, an encoder bug. The strings that remain in either surface
are data: function names and ids, environment tokens, chunk names, and the
best-effort value path of `SaveError::UnregisteredFunction`.

`error()`'s value rule is the opposite of `tostring`'s for strings: a string
passed to `error()` is the error value itself and never goes through the
shared string metatable's `__tostring`. `State::tostring_rendering` owns the
`tostring` rule and `State::error_value`, beside it, the `error()` rule.

`error()` records the position prefix (`ScriptError::prefix`) from its level
for a string value and `PrefixLocation::Suppressed` for any other value, as
Lua 5.4 adds position information only to strings. `Display` follows the
recorded policy, so a host that builds a `ScriptError` itself chooses its own.

## Argument error names

`ArgError` stores an optional function name. Its display includes that name as
`bad argument #N to 'name' (...)`; when no name was supplied or resolved, it
prints `<anonymous>`.

When the failing callback was invoked by a method call (`obj:name()`, marked in
the call instruction), the display counts arguments without the receiver and
calls a bad first argument `calling 'name' on bad self (...)`, as reference Lua
does. `ArgError::arg_number` keeps the callee's own position. The innermost
callback that returns the error decides, so an outer method call that merely
propagates an error from a nested callback does not renumber it.

The host-facing `State::check_any` and `State::check_type` helpers set the
function name only when validation fails. They ask the active Rust callback for
its name while that callback is running. `check_optional_type` uses the same
path when a present, non-nil value has the wrong type. Other errors returned
directly by a callback are not given a function name automatically.

The lookup uses the active Rust function's function-table entry in this order:

1. Scan current globals in their `IndexMap` order and return the first global
   whose value is the same entry.
2. With the `snapshot` feature, if no global matches, use the entry's
   registered stable id. An entry has at most one id.
3. If neither lookup finds a name, leave the argument error anonymous.

This is best-effort naming. Global aliases use the first matching global, and
rebinding or removing globals can change the result. The snapshot registry is
not present without the `snapshot` feature, so a callback held only in a table
or otherwise registered by id can have a name in a snapshot build and no name
in a build without that feature. The lookup compares entries, never code
addresses, so the name does not depend on what the optimizer or linker merged.

The standard-library argument validators that use `check_any_named`,
`check_type_named`, `check_optional_type_named`, or `argument_error` explicitly
set their library function name. Those errors do not depend on the callback
lookup above. This includes the current `math.*` validators.

## Stack trace names

Stack trace frames for Lua code use the function name stored in bytecode. An
unnamed frame is displayed as `main chunk` only for a main chunk; any other
unnamed function is `function <source:line>` with the line it was defined on
(`StackFrame::defined_line`, 0 for a main chunk). Snapshots store each
function's definition line, so labels survive save and load. Rust callbacks are not traceback
frames, so a callback called by Lua appears only through its Lua caller frames.
This traceback naming is separate from the optional function name in an
`ArgError`.

## Panics and poisoning

Errors are values; a panic is not an error and is never recoverable. A panic
that unwinds through VM execution skips every pop and restore on its way out,
so the call stack, stack bottom, pending bases, transient roots and open
upvalues stop describing the live frames. The State marks itself poisoned
instead of trying to make each site unwind-safe:

- Marking happens in `invoke_rust_fn` (every Rust callback), in the unwind
  guards of `with_rooted_values`, `with_restricted_env` and the vararg frame,
  and in `State::guarded`, the boundary of every public entry point that can
  execute Lua, metamethods or host callbacks (`call`, `call_anchor`,
  `get_table`, `to_string_with_meta`, `tostring_bytes`,
  `with_restricted_env`). Nothing is added to the dispatch loop or the
  recursive Lua call path.
- A poisoned State refuses every public `Result`-returning operation with
  `ErrorKind::StatePoisoned` (`SaveError::StatePoisoned` for `save_state` and
  function registration, `LoadError::StatePoisoned` when a `load_state` setup
  closure returns success after a panic). `gc_collect` becomes a no-op.
  Dropping a poisoned State is always safe. Panics from caller-supplied
  `AsRef` arguments or `user_data` destructors touch no VM bookkeeping and do
  not poison.
- A callback that catches a panic and returns normally does not get its
  result processed. `invoke_rust_fn` unwinds with a private marker payload so
  that the still-live outer frames run none of their ordinary error cleanup
  against the abandoned bookkeeping; `State::guarded` turns the marker into
  `StatePoisoned` for the host. That translation is only correct because the
  guard returns straight to host code, so VM-internal callers use the
  unguarded bodies (`call_unguarded` and kin), never the public entry points.
- Under `panic = "abort"` there is no unwind and nothing to mark.

The same entry check, `State::check_usable` (`check_save_usable` for the
snapshot methods, which report through `SaveError`), then refuses an
unsupported floating-point environment with
`ErrorKind::UnsupportedFloatEnvironment`; poison takes precedence.
`scripts/poison_contract.py` checks that every public fallible method starts
with it. Unlike poisoning, that refusal leaves the State usable once the host
restores its environment (`docs/determinism.md`, "Floating-point
environment").
