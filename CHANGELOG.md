# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Added

- Optional `snapshot` feature: `State::save_state()` / `State::load_state()`
  snapshot and restore the persistent world of a *quiescent* VM - globals
  (including shadows of builtins), the reachable table/closure/upvalue/string
  graph (cycles and shared upvalues preserved), the RNG stream, and cost
  counters - via an in-crate deterministic binary codec (no
  `serde`/`bincode`/`postcard`). It is a data snapshot, not a continuation: no
  call stack, PC, anchor, callback, or host user-data survives a load.
  Reachable `RustFunc`s must carry a stable id (`set_global_named_rust_fn`,
  `register_rust_fn`, `push_named_rust_fn`; the stdlib registers its own) or the
  save fails fast with `SaveError::UnregisteredFunction`. Saves are byte-stable
  and portable across builds sharing the same ids; environment objects
  (`math`/`string`/`table`/`_G`) and stdlib functions are rebuilt on load and
  referenced by token, so old saves see the current stdlib. A save records
  whether the source had the standard environment, so a snapshot of
  `State::empty()` round-trips as empty rather than merging in the stdlib on
  load (`FORMAT_VERSION` is 2). Dynamic-call and table-constructor base stacks
  unwind on a frame error, keeping the State quiescent so a host that catches
  an error can still snapshot or reuse it.
- The lexer accepts Lua 5.2 hex-float literals (`0x1.8p+0`, `0x.8`, `0x1p-2`),
  so numeric `string.format("%q")` output is re-parseable by dellingr itself
  (closing C30). Hex literals now convert through the shared numeral parser,
  which also rounds oversized hex integers to the nearest f64 like reference
  Lua instead of failing past `u128` range; malformed forms (`0x.`, `0x1p`,
  `0x1p+`) are a syntax error.

### Changed

- Bumped the `hotpath` profiling dependency (used by the `hotpath` feature and
  the `examples/hotpath.rs` bench harness) from 0.15 to 0.21.1.
- The VM RNG is now an in-crate SplitMix64 (`VmRng`); the `rand` dependency was
  dropped. Its state is a single `u64`, which is what lets it round-trip exactly
  in a save. This shifts the `math.random` stream for *every* build, not just
  `snapshot` (acceptable pre-1.0; the project owns its determinism baseline); a
  test pins the exact stream per seed so it cannot drift silently.
- The CLI's static-analysis line is relabeled `Minimum cost` to `Static cost
  estimate`. `analyze_cost` sums every nested body once and counts each
  loop/branch body once, so it is a structural estimate, not a runtime bound in
  either direction; the old label (and the README's "static worst-case") were
  both wrong.

### Fixed

- Pattern backreferences (`%1`-`%9`) no longer hang or corrupt memory. The
  compile-time validator infinite-looped on any backreference (rewinding onto
  the `%` instead of past the digit), and the matcher wrote candidate bytes
  *into* the earlier capture through a `*const`-to-`*mut` cast. Matching now
  compares byte ranges per Lua 5.2/5.4 and never mutates the subject.
- Closures capture the correct upvalues at scope boundaries. The compiler
  omitted `CloseUpvalues` when leaving a `do`/`if` block, at each `for`
  iteration (numeric and generic), and on `break`, so a captured local's slot
  could be reused and observed with a later value. Blocks now close on exit,
  `for` loops close the loop variable(s) each iteration, and `break` closes
  before jumping - matching Lua 5.2/5.4.
- The compiler rejects programs it cannot encode instead of emitting wrong
  bytecode or panicking: >254 fixed call arguments (255 is the dynamic-call
  sentinel), >255 parameters, a jump beyond signed-16-bit range, and >255
  field-assignment sites in one function each raise a `SyntaxError`.
  `Frame::jump` now accepts the full signed-16-bit backward range (`i16::MIN`).
- `State::consume_cost` no longer wraps on huge host charges. It cast the `u64`
  charge to `i64` before subtracting, so a charge above `i64::MAX` went negative
  and *raised* the budget (and `cost_used` could overflow). Saturating
  arithmetic now drives any charge toward exhaustion, never up.
- A tail `...` now expands in non-local assignment (`a, b = ...`) and
  generic-`for` setup (`for x in ... do`), matching tail calls and reference
  Lua. Previously only tail calls expanded and a tail vararg was truncated to
  one value, leaving `b` nil and `for x in ... do` on a nil iterator. The three
  multi-value sites (assignment, local declaration, generic-`for`) now share one
  checked adjustment helper.
- Table constructors follow Lua's multi-value rules: only the final list field
  expands when a call or `...`; earlier calls/varargs fix to one value
  (`#{7, f()}` was 2, now 4; `{...,99}` keeps one vararg then `99`). Dynamic
  constructors use a new `NewTableTracked` opcode recording the table's exact
  stack base, so `SetList` no longer scans by type - fixing crashes on nested
  (`{{...}}`) and table-valued-local-preceded constructors.
- Lua-pattern results match reference Lua. Position captures (`()`) return a
  1-based integer instead of panicking or an empty string
  (`string.match("abc", "()")` is `1`). End-of-subject matches work
  (`string.find("", "$")` is `1, 0`; `$`/`^$` match at end). Malformed patterns
  and invalid `gsub` replacements raise a runtime error (new
  `ErrorKind::RuntimeError`) instead of being swallowed to nil/unchanged, `%1`
  with no captures expands to the whole match, and the matcher gained a `%z`
  class and one-past-end guards in `%f`/`%b` (also resolves lead L4).
- Standard-library functions validate arguments like reference Lua. `tonumber`
  parses the Lua number grammar (surrounding whitespace, `0x` hex, hex floats),
  rejects `nan`/`inf` and Rust-only spellings, and treats a nil second arg as no
  base. `getmetatable`/`setmetatable` honor `__metatable` (returning it,
  refusing to replace a protected metatable). `table.insert` rejects wrong
  arity, `table.move` a non-table destination, `math.random` an empty interval
  or >2 args, and `table.insert`/`table.remove` out-of-range and non-integral
  positions. `pairs` uses the builtin `next`, so rebinding global `next` no
  longer breaks it.
- String literals decode Lua 5.2's remaining escapes: decimal `\ddd` (1-3
  digits, error above 255), hex `\xXX`, and whitespace-eating `\z` (across
  newlines). Unknown escapes are now a syntax error rather than kept literally,
  and numeric escapes produce raw bytes (`"\255" == "\xFF"`). Unsupported long
  strings (`[[...]]`, `[=[...]=]`) return a `SyntaxError` instead of panicking.
- Cost budgets are enforced at the exact operation boundary. Costs were batched
  and checked every 64 units, letting a script run dozens of ops past an
  exhausted budget; the batch now also flushes when it would reach or cross the
  remaining budget, and pending cost flushes before any reentrant call (function
  calls, generic-`for` iterators, `__index`/`__newindex`/`__len`) so a callee
  cannot overshoot. Dynamic `SetList` charges before mutating, and
  `analyze_cost` no longer overcounts an empty dynamic `SetList` (0, not 1).
- The CLI rejects an invalid or missing `--limit` with an error and non-zero
  exit instead of silently running with no budget; negative and zero budgets are
  accepted and applied.
- `tonumber` parses hex floats correctly at the extremes. An out-of-`i32`-range
  binary exponent returned nil instead of inf/`+0.0`
  (`tonumber("0x1p2147483648")`), and a long mantissa overflowed to inf and
  scaled to NaN. Parsing now keeps 16 significant hex digits with a sticky bit,
  rounds to nearest-even, handles subnormals directly, and saturates exponent
  arithmetic (overflow to inf, underflow to `+0.0`, never NaN) - bit-exact
  against lua5.2/5.4.
- `string.gsub`/`string.gmatch` match reference Lua for anchors, replacements,
  and closures. A leading `^` no longer re-anchors `gsub` to every remainder
  (`gsub("aa", "^a", "X")` is `"Xa", 1`), and `gmatch` treats a leading `^` as a
  literal caret. `gsub` validates the replacement type before matching (a
  nil/boolean repl raises `bad argument #3`), a zero limit returns the input
  without compiling the pattern, and a function/table result must be
  string/number. `gmatch` returns a single self-contained closure, so storing
  and calling the first return directly works.
- Out-of-bounds reads in the compile-time pattern validator are fixed. `%b`,
  `%f`, `[`-class, and trailing-`(` handling each read one past the pattern end
  on inputs reachable straight from `find`/`match`/`gsub`/`gmatch`; every arm now
  bounds-checks before dereferencing and reports Lua's malformed-pattern error.
  Verified UB-free under Miri.
- The lexer is hardened against long comment runs and bare `\r`. Consecutive
  comment lines were parsed by tail recursion and could overflow the stack on a
  long run; they now skip inline in a loop. A bare `\r` (old-Mac ending, or one
  left after `\z`) now advances the line counter, fixing error and stack-trace
  positions.
- `table.sort` charges its cost before sorting, not after. At an exhausted
  budget the comparator (with its side effects) and the in-place sort used to run
  and only then fail; the charge now lands after the array copy and before any
  comparator call or mutation. Empty sorts still cost 1.
- Call boundaries enforce the 255 argument/result ceiling instead of silently
  wrapping a `u8` count. `f(0, table.unpack(t255))` (256 args) arrived as 0 and
  `return 0, table.unpack(t255)` (256 results) collapsed to zero; every dynamic
  boundary now raises a clean Lua error. `unpack`/`table.unpack` reject a span
  that would overflow the count (`table.unpack({}, 0, 1e300)`). Public
  `State::call` returns `InvalidStackIndex`/`InternalError` instead of panicking
  on malformed host input, leaving the stack untouched.
- `on_error` now fires for a Rust function that fails when a host calls it
  directly through `State::call` with no Lua frame on the stack. Previously the
  identical failure reached from Lua notified but the host-direct path did not;
  it now fires exactly once per error surfaced through `call()`.
- `with_restricted_env` restores the original globals/builtins even if the
  closure panics. The restore ran under `catch_unwind` and resumes the unwind, so
  a caller that catches the panic and reuses the `State` no longer finds it stuck
  in the restricted environment.
- Interned strings count toward the GC threshold. Automatic GC compared only the
  object count, so a string-only workload (concatenation, host `push_string`
  loops) never tripped the trigger and the heap grew unbounded. A shared
  allocation count (objects plus distinct interned strings) now drives both the
  trigger and the post-collect threshold, preserving the ~2x-live-heap growth
  policy.
- Character classes close like reference Lua: at least one class byte is
  consumed before `]` can end the class. `[]]` is now a class containing `]`
  (it matched nothing before), `[^]]` matches any byte except `]`, and `[]` /
  `[^]` raise "malformed pattern (missing ']')" instead of parsing as an empty,
  never-matching class. Both the compile-time validator and the matcher's
  `classend` switched to the reference do-while together.
- An explicit `gc_collect()` no longer re-enables automatic GC after
  `gc_disable_auto()`. Every collection recomputed the threshold from the
  surviving allocation count, clobbering the `usize::MAX` disable sentinel, so
  a host that disabled automatic GC and then collected once was silently
  opted back in. Collections now preserve the sentinel; setting a finite
  threshold via `gc_set_threshold` restores adaptive recomputation.
- `string.format` follows Lua 5.4's error contract and conversion set (new
  `lua_std/string_format` module). Missing or wrong-typed arguments and unknown
  conversions raise the exact reference payloads (`no value`, `number expected,
  got string`, `invalid conversion '%y' to 'format'`, `invalid conversion
  specification: '%100d'`, ...) instead of being silently skipped or passed
  through literally. All 5.4 conversions are implemented - `%c %d %i %u %o %x
  %X %e %E %f %g %G %a %A %s %q %p` - with per-conversion flag validation,
  two-digit width/precision limits, and the 22-byte directive cap. Integer
  conversions accept an f64 only when finite, integral, and exactly
  representable in i64 (so `%u`/`%x` of -1 give the wrapped u64 forms), `%s`
  honors `__tostring` byte-exactly and keeps embedded NUL when unmodified, and
  `%q` emits 5.4's literal forms (numbers take the hex-float branch: `1.5` is
  `0x1.8p+0`, NaN is `(0/0)`, inf is `1e9999`; dellingr's own lexer cannot yet
  re-parse hex-float literals - recorded as C30). `%p` stays deterministic:
  `(null)` for non-object values and a stable state-local id for
  strings/tables/functions instead of a real address.

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
