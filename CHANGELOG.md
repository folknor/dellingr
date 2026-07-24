# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Added

- Optional `snapshot` feature: `State::save_state()` / `State::load_state()`
  snapshot and restore the persistent script world of a *quiescent* VM -
  globals (including user shadows of builtin names), the reachable
  table/closure/upvalue/string graph (cycles and shared upvalues preserved),
  the RNG stream, and cost counters - through a small in-crate deterministic
  binary codec (no `serde`/`bincode`/`postcard`). It is a data snapshot, not a
  continuation: no call stack, program counter, coroutine, anchor, callback, or
  host user-data handle survives a load. Reachable `RustFunc`s must be
  registered with a stable id (`set_global_named_rust_fn`, `register_rust_fn`,
  `push_named_rust_fn`; the stdlib registers its own), or the save fails fast
  with `SaveError::UnregisteredFunction`. Saves are byte-stable and portable
  across builds of the same binary that register the same ids. Environment
  objects (`math`/`string`/`table`/`_G`) and stdlib functions are rebuilt on
  load and referenced by token, so old saves see the current stdlib.

### Changed

- Bumped the `hotpath` profiling dependency (used by the `hotpath` feature and
  the `examples/hotpath.rs` bench harness) from 0.15 to 0.16.1.

- The VM RNG is now an in-crate SplitMix64 (`VmRng`) and the `rand` dependency
  was dropped entirely. Its entire state is a single `u64`, which is what lets
  the RNG round-trip exactly in a save. This changes the `math.random` stream
  for *every* build, not just `snapshot` (acceptable pre-1.0; the project
  owns its determinism baseline). The exact stream for a given seed is pinned
  by a test so it cannot drift silently.

### Fixed

- Pattern backreferences (`%1`-`%9`) no longer hang or corrupt memory. The
  compile-time pattern validator infinite-looped on any backreference (it
  rewound onto the `%` instead of advancing past the digit), and the matcher
  wrote candidate bytes *into* the earlier capture through a `*const`-to-`*mut`
  cast instead of comparing them. Backreference matching now compares byte
  ranges per Lua 5.2/5.4 semantics and never mutates the subject.
- Closures now capture the correct upvalues at lexical-scope boundaries. The
  compiler was omitting `CloseUpvalues` when leaving a `do`/`if` block, at each
  `for`-loop iteration (numeric and generic), and on `break`, so a captured
  local's stack slot could be reused and the closure would observe a later
  value. Blocks close on exit, `for` loops close the visible loop variable(s)
  each iteration, and `break` closes before jumping - matching Lua 5.2/5.4.
- The compiler now rejects programs it cannot encode instead of emitting wrong
  bytecode or panicking: more than 254 fixed call arguments (255 is the
  dynamic-call sentinel), more than 255 function parameters, a control-flow
  jump beyond the signed 16-bit range, and more than 255 field-assignment
  sites in one function each raise a clear `SyntaxError`. `Frame::jump` now
  accepts the full signed-16-bit backward range (`i16::MIN`).
- `State::consume_cost` no longer wraps on very large host charges. It cast the
  `u64` charge to `i64` before subtracting, so a charge above `i64::MAX` went
  negative and *raised* the remaining budget (and `cost_used` could overflow).
  It now uses saturating arithmetic: any charge, up to `u64::MAX`, drives the
  budget toward exhaustion and never increases it.
- A tail `...` is now expanded in non-local assignment (`a, b = ...`) and in
  generic-`for` setup (`for x in ... do`), matching the existing behavior for a
  tail function call and reference Lua. Previously only a tail call was
  expanded and a tail vararg was truncated to one value, so `a, b = ...` left
  `b` nil and `for x in ... do` failed on a nil iterator state. The three
  multi-value sites (assignment, local declaration, generic-`for`) now share
  one checked adjustment helper.
- Table constructors follow Lua's multi-value rules: only the final list field
  expands when it is a call or `...`, and every earlier call/vararg is fixed to
  one value. Previously a final call was forced to one result (`#{7, f()}` was
  2, now 4) and a non-final `...` expanded to all values (`{...,99}` now keeps
  one vararg then `99`). Dynamic constructors use a new `NewTableTracked`
  opcode that records the table's exact stack base, so `SetList` no longer
  scans for the table by type - fixing crashes on nested dynamic constructors
  (`{{...}}`) and on a constructor preceded by a table-valued local.
- Lua-pattern result handling now matches reference Lua. Position captures
  (`()`) return a 1-based integer instead of panicking or yielding an empty
  string (`string.match("abc", "()")` is `1`). End-of-subject matches work:
  `string.find("", "$")` is `1, 0` and `$`/`^$` match at the end of a string.
  Malformed patterns and invalid `gsub` replacements now raise a runtime error
  (via the new `ErrorKind::RuntimeError`) instead of being silently swallowed
  to nil / unchanged input, and `%1` with no explicit captures expands to the
  whole match. The matcher gained a `%z` class (matching NUL) and guards
  against one-past-end reads in `%f`/`%b` (this also resolves lead L4).
- Standard-library functions now validate their arguments like reference Lua.
  `tonumber` parses the Lua number grammar (leading/trailing whitespace, `0x`
  hex, hex floats) and rejects `nan`/`inf` and Rust-only spellings, and treats
  a nil second argument as no base. `getmetatable`/`setmetatable` honor a
  `__metatable` field (returning it, and refusing to replace a protected
  metatable). `table.insert` rejects wrong arity, `table.move` rejects a
  non-table destination, `math.random` errors on an empty interval (and on more
  than two arguments), and `table.insert`/`table.remove` reject out-of-range and
  non-integral positions. `pairs` uses the builtin `next` iterator, so
  rebinding the global `next` no longer breaks it.

## [0.3.0] - 2026-05-12

### Added

- Added `examples/strings/literal_find.lua` to track literal
  `string.find` performance through the examples and hotbench paths.
- Added `examples/alloc/record_tables.lua` to track larger record-shaped
  table constructor allocation and promotion costs.
- Added a `--quiet` / `-q` CLI flag to suppress the `Cost used` summary
  for harnesses that run many short-lived scripts.

### Fixed

- `table.move` now handles overlapping same-table moves that shift the
  source range to the right without clobbering unread values.
- `select` now handles negative indices by counting from the end and
  rejects zero or too-negative indices instead of returning all args.
- Global `unpack` now supports the optional `i, j` range arguments,
  matching the already-supported `table.unpack` behavior.
- `table.concat` now rejects non-string and non-number elements instead
  of coercing every Lua value with display-string semantics.
- `tonumber` now supports the optional base argument for bases 2 through
  36, including signed strings and invalid-digit nil results.
- Unparenthesized Lua calls with a literal string or table constructor
  argument now compile instead of panicking in the parser.
- `State::set_top` now accepts negative stack indices instead of
  panicking for otherwise valid relative-top operations.
- Table constructors now accept identifier-starting array entries like
  `{x}` and `{x.y}` instead of treating every identifier as `name =`.
- Empty string patterns now match Lua boundary behavior in `string.find`,
  `string.match`, `string.gmatch`, and `string.gsub`.
- Dotted method declarations like `function mod.sub:run()` now compile
  and bind the method on the final receiver table.

### Performance

- `string.find` now uses the plain substring path for patterns with no
  magic characters even when `plain` is omitted. The literal path also
  avoids copying the subject and pattern before searching.
- `string.sub` no longer copies the full source string before slicing;
  it copies only the returned byte range.
- Fresh `State` startup reserves the standard-library globals, string
  pool, and large library table capacities up front.
- String interning now uses an in-crate pinned chunked hash instead of
  `DefaultHasher`, reducing short-string hashing overhead and removing
  dependence on the standard library's unspecified hasher choice.
- `string.gsub` string replacements now append directly into the output
  buffer and patterned substitutions reuse capture storage across
  matches, reducing per-match allocation churn.
- `type`, `tonumber`, and already-string `tostring` avoid unnecessary
  Rust string allocations in common cases.
- Base standard-library Rust functions install directly into the global
  table instead of round-tripping through the Lua stack during fresh
  `State` startup.
- Standard-library module tables now register Rust functions and numeric
  constants through a direct string-key table path instead of pushing
  each key/value pair through the Lua stack.
- Table constructors with more than four static fields now emit
  `NewTablePresized`, allocating map-backed storage at the final
  constructor size instead of promoting after the inline table fills.
- Larger pure named-field table constructors now use bytecode-level
  key templates and pinned field initialization, reducing repeated
  key lookup work while preserving nil-removal behavior.

### Testing

- Split the Rust differential harness into smaller per-test buckets so
  `brokkr check` stays under its 20s per-test timeout. `diff_test.sh`
  now accepts explicit file or directory subsets while keeping its
  default full example sweep.
- The recursive example integration test now runs the already-built
  `dellingr` binary and splits hotpath examples into per-file tests,
  avoiding repeated `cargo run` invocations and the same per-test
  timeout pressure.
- The differential and recursive example harnesses now run `dellingr`
  with `--quiet`, avoiding one extra output line and filter pass per
  process.
- Rust differential test buckets now build the release `dellingr`
  binary once and reuse it, instead of invoking a release build from
  every bucket.
- Rust differential test buckets now rely on brokkr's per-test timeout
  instead of wrapping every `dellingr`, `lua5.2`, and `lua5.4` process in
  `timeout(1)`.
- `diff_test.sh` now uses process-unique temporary output files so
  parallel Rust differential buckets do not race on shared `.diff_test_*`
  paths.
- The hotpath benchmark harness now reports `state_new_us`, making fresh
  `State` startup cost visible alongside parse and call timings.
- Table constructor tests now cover templated nil-field removal and
  duplicate named-field fallback behavior.
- The hotpath benchmark harness now reports setup-only key/value output
  for scripts without a `_bench` function, and `hotbench.sh` preserves
  setup/final heap and object counters in its filtered output.

## [0.2.0] - 2026-05-07

### Added

- `Anchor`: retain Lua values across host calls without touching globals.
  See `State::anchor*`, `push_anchor`, `call_anchor`, `release_anchor`.
  ([6837ccb](https://github.com/folknor/dellingr/commit/6837ccb))
- `Engine` / `Program`: compile Lua source once, share the immutable
  `Program(Arc<Bytecode>)` across many `State`s. `State::load(&Program)`
  loads it as a callable closure.
  ([e0bef7b](https://github.com/folknor/dellingr/commit/e0bef7b))

### Changed (breaking)

- `State::set_table_raw` argument order now matches `lua_rawset`: value
  on top of the stack, key below. Previously reversed.
  ([eeeb4ad](https://github.com/folknor/dellingr/commit/eeeb4ad))
- `State` is now `Send` (not `Sync`). `HostCallbacks: Send` and the
  user-data slot is `Box<dyn Any + Send>` - any `Rc` / `RefCell` in
  callbacks or user data must become `Arc` / `Mutex`.
  ([e0bef7b](https://github.com/folknor/dellingr/commit/e0bef7b))

### Fixed

- Method-dispatch inline caches no longer survive a `string` (or other
  builtin) rebind or a `with_restricted_env` swap. The cached library
  pointer used to bypass the new binding for the lifetime of the cached
  closure - notably, a host could pre-warm a `s:method()` call site
  outside the sandbox and the cached method would still resolve inside.

### Performance

- String-heavy workloads ~2x faster: O(1) interner, chained `..` lowers
  to one `OP_CONCAT(n)`, concat buffer pre-sized.
- Inline caches now cover `obj:method()` and `s:method()` dispatch on
  top of field reads and writes.
- Numeric integer indexing fast path on hash-storage tables
  (`tables/numeric_index` 112ms -> 73ms).

## [0.1.0] - 2026-05-05

Initial release.
