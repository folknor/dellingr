# Error reporting and function names

Errors carry a structured `ErrorKind`, source location, and an optional stack
trace. Their `Display` text is for diagnostics. It is not a stable identifier
for an error or a cross-build log format.

The built-in `string.gmatch` wrapper and iterator shim are omitted from runtime
error traces and host callback source context when a script caller exists:
lazy pattern errors use the script caller's call-site line, and host callbacks
receive the caller's source context. With no script caller, the shim keeps its
own location and source context. Recognition uses the shared bytecode
identities for the outer wrapper and its iterator, so a user chunk named
`string.gmatch` retains ordinary attribution. Snapshot load gives a restored
iterator the shared identity: a chunk qualifies only when a closure of it
captures the private gmatch iterator function and its contents equal this
build's shim. An iterator saved by a build whose shim differs keeps its own
bytecode and location.

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
`bad argument #N to 'name' (...)`; when no name was supplied, it prints
`<anonymous>`. An argument check made while no Rust function is running (from
the host, outside any callback) has no name.

When the failing callback was invoked by a method call (`obj:name()`, marked in
the call instruction), the display counts arguments without the receiver and
calls a bad first argument `calling 'name' on bad self (...)`, as reference Lua
does. `ArgError::arg_number` keeps the callee's own position. The innermost
callback that returns the error decides, so an outer method call that merely
propagates an error from a nested callback does not renumber it.

An error raised by `State::raw_field` is an `ArgError` on the table argument
whose `ArgError::field` is the failing field name. The display prefixes the
detail with `field 'name': ` inside the usual parentheses (`bad argument #1 to
'spawn' (field 'x': number expected, got string)`, or `calling 'spawn' on bad
self (field 'x': ...)` for a method call), and `expected` and `received`
describe the field, not the argument. `ArgDetail::ValueOutOfRange` and
`ArgDetail::InvalidUtf8` cover an integral number outside the target integer
type and a string that is not valid UTF-8.

The host-facing `State::check_any` and `State::check_type` helpers set the
function name only when validation fails. They ask the active Rust callback for
its name while that callback is running. `check_optional_type` uses the same
path when a present, non-nil value has the wrong type. Other errors returned
directly by a callback are not given a function name automatically.

The name is the active Rust function's own diagnostic name, a property of its
function-table entry fixed when the entry was created:

- A host entry takes the name given to `State::add_rust_fn(name, f)` or
  `State::register_rust_fn(id, name, f)`. `State::set_global_named_rust_fn`
  uses the global name it installs.
- A standard-library entry takes its catalog id (`print`, `string.find`,
  `math.sin`), in every build.

Nothing is discovered at error time. The name does not depend on which globals
exist, on what the script calls the function, on whether it is held only in a
table (`game.spawn`), or on the `snapshot` feature, so a function reached under
several aliases renders one name, and a function never bound as a global is no
longer anonymous. Names need not be unique: two entries with one name are still
two distinct values. A name is not saved (a load takes names from the entries
its setup registers, so a newer build can rename a function without breaking
saves) and is never part of a function's identity or saveability. A snapshot id
is not a fallback name. The lookup compares entries, never code addresses, so
nothing depends on what the optimizer or linker merged.

Attribution follows dispatch. An error raised inside a nested Rust callback
names the inner function; validation in the outer callback after a nested
`State::call` returns names the outer one. A plain Rust helper called directly
by a callback, not dispatched through the VM, is attributed to that callback.

The standard-library argument validators that use `check_any_named`,
`check_type_named`, `check_optional_type_named`, or `argument_error` explicitly
set their library function name (`sub`, not `string.sub`), which applies on top
of the entry name. This includes the current `math.*` validators.

## Stack trace names

Stack trace frames for Lua code use the function name stored in bytecode. An
unnamed frame is displayed as `main chunk` only for a main chunk; any other
unnamed function is `function <source:line>` with the line of its `function`
keyword (`StackFrame::defined_line`, 0 for a main chunk). Snapshots store each
function's definition line, so labels survive save and load. Rust callbacks are
not traceback frames, so a callback called by Lua appears only through its Lua
caller frames. This traceback naming is separate from the optional function
name in an `ArgError`.

A location renders only what is known. A frame or error prefix of an unnamed
chunk shows just the line (`3: in main chunk`, `3: msg`); a line of 0 is
omitted, and an error with neither a source name nor a line, such as host API
misuse built with `Error::without_location`, has no prefix. A column appears
only when it is known for the selected site. There is no `[C]` marker: an
absent source name never means a native frame.

## Panics and poisoning

Errors are values; a panic is not an error and is never recoverable. A panic
that unwinds through VM execution skips every pop and restore on its way out,
so in-flight execution bookkeeping no longer describes the live frames. The
State marks itself poisoned instead of trying to make each site unwind-safe:

- Poison marking is owned by the unwind boundaries in `invoke_rust_fn`,
  `with_rooted_values`, `with_restricted_env` and `State::guarded`. Public
  entry points that can execute Lua, metamethods or host callbacks use
  `State::guarded`. Nothing is added to the dispatch loop or the recursive Lua
  call path.
- A poisoned State refuses every public `Result`-returning operation with
  `ErrorKind::StatePoisoned` (`SaveError::StatePoisoned` for `save_state`,
  `LoadError::StatePoisoned` when a `load_state` setup
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

`State::check_usable` (`check_save_usable` for the snapshot methods, which
report through `SaveError`) refuses a poisoned State before other work and
then refuses an unsupported floating-point environment with
`ErrorKind::UnsupportedFloatEnvironment`; poison takes precedence.
`scripts/poison_contract.py` checks this entry rule for every public fallible
method. Unlike poisoning, an unsupported-environment refusal leaves the State
usable once the host restores its environment (`docs/determinism.md`,
"Floating-point environment").
