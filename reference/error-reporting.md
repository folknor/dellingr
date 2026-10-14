# Error reporting and function names

Errors carry a structured `ErrorKind`, source location, and an optional stack
trace. Their `Display` text is for diagnostics. It is not a stable identifier
for an error or a cross-build log format.

## Argument error names

`ArgError` stores an optional function name. Its display includes that name as
`bad argument #N to 'name' (...)`; when no name was supplied or resolved, it
prints `<anonymous>`.

The host-facing `State::check_any` and `State::check_type` helpers set the
function name only when validation fails. They ask the active Rust callback for
its name while that callback is running. `check_optional_type` uses the same
path when a present, non-nil value has the wrong type. Other errors returned
directly by a callback are not given a function name automatically.

The lookup uses the active Rust function pointer in this order:

1. Scan current globals in their `IndexMap` order and return the first global
   whose value is the same Rust function pointer.
2. With the `snapshot` feature, if no global matches, look up the function
   pointer in the reverse stable-id registry. When multiple ids share an
   address, registration retains the lexicographically smallest id for this
   reverse lookup.
3. If neither lookup finds a name, leave the argument error anonymous.

This is best-effort naming. Global aliases use the first matching global, and
rebinding or removing globals can change the result. The snapshot registry is
not present without the `snapshot` feature, so a callback held only in a table
or otherwise registered by id can have a name in a snapshot build and no name
in a build without that feature. The lookup also compares function-pointer
addresses. Link-time optimization or identical-code folding can give distinct
callbacks the same address, changing which matching global or registered id
is reported between builds.

The standard-library argument validators that use `check_any_named`,
`check_type_named`, `check_optional_type_named`, or `argument_error` explicitly
set their library function name. Those errors do not depend on the callback
lookup above. This includes the current `math.*` validators.

## Stack trace names

Stack trace frames for Lua code use the function name stored in bytecode. An
unnamed frame is displayed as `main chunk`. Rust callbacks are not traceback
frames, so a callback called by Lua appears only through its Lua caller frames.
This traceback naming is separate from the optional function name in an
`ArgError`.
