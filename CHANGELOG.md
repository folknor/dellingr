# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## Unreleased

### Added

- Added `examples/strings/literal_find.lua` to track literal
  `string.find` performance through the examples and hotbench paths.
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
