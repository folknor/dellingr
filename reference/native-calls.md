# Calls back into Lua: the native-stack guard

Every call made from Rust into a function runs on the native stack and
counts against `MAX_NATIVE_CALL_DEPTH`. What one such level costs is the sum
of the fixed frames on the compiled path that made the call: the dispatch
loop, the opcode handler, the metamethod or library code between them, and
`call_unguarded`. docs/limits.md, "Envelope", promises that every limit at
once fits a stated thread size, and `tests/native_stack.rs` backs that
promise by running each such path to the limit. A path the test does not
know about is a path the promise does not cover, so the set of paths is
kept current mechanically:

- `reference/native-calls.toml` is the inventory: every function that can
  reach a call back into Lua, every call site between them (an edge), and
  every scenario of the test with the ordered edges of its path.
- Every call to an inventoried function in `src/` carries a
  `// native-call: <edge-id>` comment naming its edge.
- `scripts/native_calls.py` checks the inventory against the source and the
  test and prints `ok` when they agree; `brokkr check` runs it as the
  `native-call-inventory` script check.
- The `native-call-marker` textlint rule in `brokkr.toml` flags any call to
  an inventoried name without a marker, so an editor or a plain
  `brokkr check` points at the line before the script's report is read.

Markers are comments. Adding one never changes code: frames that stay live
across a native re-entry are size-sensitive (AGENTS.md, "Hotpath
benchmarks"), and `brokkr test native_bytes_per_level` in the dev and
release profiles must report the same numbers before and after.

## The inventory

The functions are the caller closure of the two execution boundaries:
`State::call_unguarded`, which every call made from Rust goes through, and
`State::invoke_rust_fn`, which runs every Rust callback. The closure stops
only at roots, functions that are also entered from outside the lexical call
graph, each listed with its kind and the evidence for it:

- `callback`: a library callback, entered through a function pointer by
  `invoke_rust_fn`. A callback written as a closure in a registration list
  (`("name", |state| { ... })` or `add!("name", |state| { ... })`) is a
  function of its own named `lib:name`, prefixed with its library
  (`lib:table.sort`; the base library has no prefix, `lib:print`).
- `host-api`: a `pub fn` on `State` that a host, or a host callback, calls.
- `binary`: the CLI's `main`.

An edge id is `caller->callee`, with a `:tag` when one caller calls one
callee from several sites (`handle_index_metamethod_inner->call_unguarded:lua`
and `...:rust`). Each edge is either covered or excluded:

- covered by the scenarios whose path runs it, or by the `chains` or
  `leaves` it names (the metamethod chain kinds and deepest-point work in
  `tests/native_stack.rs`, for edges that only those reach);
- or `excluded`, with the reason the call can never reach Lua from that site
  (the default sort comparison, a raw `table.move` copy, the `%%` directive)
  or why it lies outside the envelope (the CLI's own call).

A scenario's path is one native level: it starts at an edge out of
`call_unguarded` and ends with an edge into it. Consecutive edges must
connect, except that an edge into `invoke_rust_fn` may be followed by an
edge out of a `callback` or `host-api` root, the function pointer the
callback was entered through. Named `[segment]` runs of edges keep shared
prefixes in one place; `@name` in a path expands one.

## What the script checks

- The inventoried functions are exactly the computed caller closure, and a
  function without callers in `src/` is a classified root whose evidence
  holds: a callback has the `RustFunc` signature and is used as a value, a
  host-api root is a `pub fn` on `State`, a binary root is `src/main.rs`'s
  `main`.
- Every call to an inventoried function has exactly one marker, on its line
  or the line directly above, and the marker's caller and callee are the
  enclosing function and the called one. Every marker belongs to a call,
  every edge id marks exactly one call, and every call site of one pair is
  tagged when there are several.
- Every edge is covered or excluded, never both; every chain and leaf an
  edge names exists; every scenario path connects.
- The scenarios in the inventory are exactly the `SCENARIOS` list of
  `tests/native_stack.rs`, and the chain and leaf names exist in its
  `CHAINS` and `LEAVES` lists.
- The textlint rule's pattern and `except` equal the inventory's names, and
  nothing outside the rule's `exclude` matches the pattern without being a
  call to an inventoried function. `src/compiler/` is excluded because it
  holds no State and its `Instr::call`, `get_table` and `set_table`
  constructors share names with inventoried functions; a call into an
  inventoried function from an excluded file is reported.

## The lexical model and the direct-call convention

The script reads source text, not a compiler call graph. It finds `fn`
items and their bodies, skips `#[cfg(test)]` items and test files, and
attributes a closure's body to the function that contains it, so a call
inside `self.guarded(|state| ...)` or `with_metamethod_depth(|state| ...)`
belongs to the enclosing function. Higher-order helpers that only invoke
such a closure (`guarded`, `with_metamethod_depth`, `with_rooted_values`)
are frames on the path but not inventoried: the scenarios run them.

A call resolves by name and receiver. That is the convention re-entering
functions are called by, and new code should keep to it:

- `self.f(...)` inside `impl T`, and `Self::f(...)`, resolve to `T::f`;
- `state.f(...)` resolves to `State::f`, so a `&mut State` parameter is
  named `state`;
- `T::f(...)` resolves to `T::f`;
- a bare or module-path `f(...)` resolves to a free function;
- any other receiver resolves to every method named `f`, which is safe but
  may ask for markers on calls that cannot re-enter.

Method chains split over lines keep the name and its `(` on one line, as
rustfmt writes them, so the marker goes on the line of the name or directly
above it.

Calls through function pointers, closures and trait objects are invisible to
a lexical model, so each kind has a fixed answer. Rust callbacks are entered
only through `invoke_rust_fn`, which is a boundary. A closure handed to a
helper is attributed to the function that wrote it. A host closure
(`with_restricted_env`, the snapshot loader's setup) is host code, which
reaches Lua through the `host-api` roots. The table sort's
`TableSortAccess` implementations read and write raw. A new indirect call
that can reach Lua needs this guard extended before it lands.

## Adding a path

When a change adds a call that can reach Lua, `brokkr check` reports the
unmarked call (textlint and script) and, for a new re-entering function,
that it is not listed. Then:

1. Add the function to `[[function]]` (with its `root` kind and evidence if
   it is entered from outside), the call site to `[[edge]]`, and the marker
   to the call.
2. If the edge opens a new path, add a `Scenario` to `tests/native_stack.rs`
   that recurses through it (its `native` and `meta` per cycle, a `DESCEND`
   inside the handler the path calls, and Lua checks of any returned value),
   and a `[[scenario]]` with the same name and its path. If only a chain
   kind or a deepest-point item reaches it, name those on the edge instead.
3. If the call can never reach Lua, or is outside the envelope, mark the
   edge `excluded` with the reason.
4. Run `brokkr test native_bytes_per_level` with `--debug` and `--release`
   to see the new path's bytes per level, and `brokkr check`, which runs the
   envelope in both profiles.

A path that leaves a dispatch arm (`run_frame->...`) must also record the
instruction as the activation's call site before anything on it can call,
or tracebacks and `print` locations name a stale line. The arms do not
record on every execution: `State::record_call_site` explains where each
existing path records, which for the table and length instructions is the
first line of their outlined slow function.

`python3 scripts/native_calls.py sites` lists every call site with its
marker, which is the quickest way to see what a change touched.
