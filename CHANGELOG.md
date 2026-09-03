# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Changed

- **Constant folding.** Literal arithmetic and unary negation now fold at
  parse time with reference lcode.c's guards (NaN and -0.0 results stay
  runtime operations). Folded operations no longer execute or charge cost,
  so `cost_used` decreases for identical source containing literal
  arithmetic like `60 * 1000` or `-5` - re-baseline stored cost
  fingerprints. Runtime results are bit-exact (the folder and the VM share
  one definition of each operation, floored modulo included).

### Fixed

- **Upvalue pool no longer grows monotonically.** Pool slots whose upvalue is
  unreachable are swept onto a free list during `gc_collect` and reused by
  later allocations, so a long-lived State no longer retains one slot for
  every closure-with-captures ever created. Reuse order is deterministic
  (LIFO, at deterministic GC points).
- **`string.format("%p")` identity map is now GC-swept.** Entries whose value
  no longer resolves on the heap are dropped during collection, bounding the
  lookup scan by live `%p` usage instead of lifetime-total usage. Assigned ids
  are never reissued.

## [0.4.0] - 2026-07-27

The bulk of this release is hardening. Roughly a dozen defects could abort the
host process from ordinary script input; all are closed, and the classes they
came from (unbounded recursion, unrooted values, untrusted save data) are now
covered structurally rather than case by case.

### Changed (breaking)

- **Cost model.** Data-dependent native work in the string, pattern, and
  `table.concat` libraries is now charged; it was previously free. There is no
  conversion formula - the increase depends on string lengths, output
  expansion, match success, captures, and backtracking shape. **Every embedder
  must re-measure its tick budgets.** `COST_MODEL_VERSION` is exported so hosts
  can identify the accounting model independently of the crate version.
- **Stack API.** `MAX_STACK_SIZE` is now a real global cap on the shared
  Lua/Rust value stack, not just a check when preparing Lua frames - previously
  a host `RustFunc` pushing in a loop never met it. Consequently `push_nil`,
  `push_number`, `push_boolean`, `push_rust_fn`, `push_bytes`, `push_string`,
  `new_table`, `get_global`, `open_libs`, `set_top` and `pop` all return
  `Result<()>`, and `State::to_string` takes `&mut self`. Exceeding the cap is a
  catchable `StackOverflow`; a rejected operation leaves the stack, string pool
  and function registry unchanged. Enforcement charges no cost.
- **String size limit.** Lua strings are capped at 16 MiB
  (`MAX_STRING_BYTES`, inclusive) with a dedicated
  `ErrorKind::StringSizeExceeded { size, limit }`, so hosts need not parse error
  text to recognise a resource-limit termination.
- **Save format.** Bumped v2 to v6; existing saves are rejected with
  `UnsupportedVersion` under a strict equality gate, with no fallback parsing.
  The intervening versions added the cost-budget-configured flag, the
  environment delta, the object-identity registry, the widened literal pools,
  and `COST_MODEL_VERSION`.
- **RNG stream.** The VM RNG is an in-crate SplitMix64 (`VmRng`) and the `rand`
  dependency is gone, so `math.random` output shifts for every build, not just
  `snapshot`. A test pins the exact stream per seed. Its single-`u64` state is
  what lets it round-trip exactly in a save.
- **Error taxonomy.** New `ErrorKind::ScriptError`: a script-raised `error()` is
  no longer reported as `InternalError`, which is documented as "corrupt
  bytecode or VM bug" - embedders filtering that for crash reporting were
  collecting ordinary script errors.
- **`tostring` output.** Heap objects render `table: 0x1` / `function: 0x2`
  instead of the slotmap key's Rust `Debug` form (whose type word was wrong for
  every object), and Rust functions render a constant `<function>` instead of a
  real pointer, which ASLR changed between runs - a replay-visible determinism
  break, since scripts can branch on the string. `Display for ObjectPtr` and
  `Display for Val` are deleted rather than left as traps.

### Added

- **Optional `snapshot` feature:** `State::save_state()` / `State::load_state()`
  snapshot and restore the persistent world of a *quiescent* VM - globals, the
  reachable table/closure/upvalue/string graph (cycles and shared upvalues
  preserved), the RNG stream, cost counters, and object identities - through an
  in-crate deterministic binary codec (no `serde`/`bincode`). It is a data
  snapshot, not a continuation: no call stack, PC, anchor, callback or host
  user-data survives a load. Reachable `RustFunc`s must carry a stable id or the
  save fails fast with `SaveError::UnregisteredFunction`. Mutations to stdlib
  tables (`math.myconst = 42`, a deleted stock entry) persist as a delta against
  the pristine environment, so extending a library table no longer silently
  loses data.
- **Source limits**, bounded and documented in the README: parser recursion caps
  at 200 levels (`TooManySyntaxLevels`) and upvalues at 255 per function
  (`TooManyUpvalues`). Previously deep nesting overflowed the native stack - an
  abort, not a returnable `SyntaxError` - and the 256th upvalue silently got
  index 0 and read the wrong variable. One accepted divergence: a paren costs
  two depth ticks, so parens nest ~99 deep against reference Lua's ~200.
- `error(msg, level)`: `0` suppresses the location prefix, `N` blames the caller
  `N-1` frames up, out-of-range renders no prefix.
- Hex-float literals (`0x1.8p+0`, `0x.8`, `0x1p-2`), which makes numeric
  `string.format("%q")` output re-parseable by dellingr itself.
- Leveled long comments (`--[=[ ... ]=]`). Long *strings* remain rejected.
- Clearing the field a `pairs` traversal is sitting on - the filter-in-place
  idiom reference Lua permits - via tombstoned removal that preserves iteration
  order and cannot resurrect a deleted field through a warm `SET_FIELD` cache.
  Costs ~2.5% on `iter/pairs`.

### Changed

- The Lua pattern matcher is rewritten as a one-pass compiler plus a safe
  index-based matcher, replacing a raw-pointer transliteration of the reference
  C. Every atom is an interned 256-bit byte class, so matching is one bitset test
  with no branch on atom kind. Error timing is unchanged and remains contract:
  compile errors from `from_bytes_try`, match-time errors from
  `matches_bytes_from`.
- `table.sort` is an iterative heap sort with a fallible comparator, replacing a
  bubble sort that always performed exactly `N(N-1)/2` comparator calls. Heap
  sort specifically because a script-controlled comparator may be inconsistent:
  every index derives from fixed heap bounds, so a comparator that always
  returns true yields a wrong-but-safe permutation rather than an
  out-of-bounds access.
- Per-function capacity ceilings are lifted: literal pools move from 8-bit to
  16-bit ids, so 300 constructor fields or 300 distinct number literals now
  compile where reference Lua accepts them. Inline-cache slots narrow to 8 bits
  with 255 meaning "uncached", so cached sites cap at 255 per function and the
  rest take the slow path - the most field-heavy example in the repo has 83.
- The `string` library accepts numbers wherever it accepts strings, matching
  `luaL_checklstring`. Arithmetic stays strict (`"10" + 1` is still an error) and
  that is now documented under "Won't implement" rather than being an
  undocumented divergence.
- `analyze_cost` reports lower `ScopeCost::instructions` for unchanged source:
  finalization strips retired call marks and no-op `CloseUpvalues`. Charged cost
  is unaffected - `own_cost`, `total_cost` and `cost_used` are identical, since
  the stripped instructions were free. Only embedders comparing raw instruction
  counts against stored baselines will see a difference.
- `math.log` special-cases base 10, so `math.log(1000, 10)` is exactly `3`. Base
  2 is deliberately left matching 5.2.
- The CLI's static-analysis line is relabeled from `Minimum cost` to `Static cost
  estimate`; `analyze_cost` is a structural estimate, not a runtime bound in
  either direction, so the old label was wrong.
- `break` is no longer terminal (`while true do break; end` compiles), and
  numerals glued to identifiers are rejected (`3or 4`), tracking 5.4.
- Zero-step numeric `for` is documented as a deliberate divergence: dellingr
  skips in both directions, where 5.2 skips ascending but loops forever
  descending and 5.4 errors.
- `rust-version` returns to the documented 1.92; the manifest had claimed 1.97,
  which arrived alongside an unrelated bump. Bumped `hotpath` 0.15 to 0.21.1.

### Fixed

- **Aborts reachable from a script.** The GC marked the heap recursively
  (~5 native frames per nesting level, unbounded); `save_state()` walked the
  object graph recursively; the lexer tail-recursed over comment runs. All three
  are now iterative, with save output byte-identical and pinned by a golden
  fixture. Four GC root holes - frame varargs, `#t` with a `__len` metatable,
  `table.sort`'s array copy, and `with_restricted_env`'s swapped globals - each
  held a live `Val` across an operation that can collect; a `TransientRoots`
  registry now covers them and `validate_quiescent` requires it to be empty.
  `next(t, 0/0)` hit an `IndexMap` hash assert that fires in release. Pattern
  backreferences infinite-looped in the validator and wrote through a
  `*const`-to-`*mut` cast; the compile-time validator also read past the pattern
  end on inputs reachable straight from `find`/`match`/`gsub`/`gmatch`. Verified
  UB-free under Miri.
- **Untrusted save data.** Save files are user-editable and the bytecode inside
  them was previously trusted completely. A verifier behind a
  `VerifiedSavePayload` (no unchecked constructor) now checks graph and chunk
  invariants, every operand used as an index, jump targets, closure capture
  arity, and acyclicity, plus operand-stack discipline - a forged save whose
  chunk began with `OP_POP` or `OP_SWAP` previously passed every structural check
  and panicked the host on load. Rejections raise `LoadError::InvalidBytecode`.
  The same checks run on compiler output as a debug assertion, so the test corpus
  continuously proves the compiler emits only what the loader accepts. Two
  save-corruption bugs in the writer were exposed by this: both the bytecode and
  upvalue arenas assigned an id before pushing the entry, so a first-seen parent
  and child received the same id and a closure could silently restore the wrong
  program.
- **Unbounded string allocation.** `OP_CONCAT` charged nothing while
  `concat_helper` did `O(total bytes)` of work, and the surrounding loop is
  deliberately free, so building a 1 MiB string reported `Cost used: 0` and
  completed under `--limit 100` - a host could not defend by lowering the budget,
  because the cost was zero at any budget. Closed by the 16 MiB cap above,
  enforced both at interner admission and in every expanding producer (concat,
  `table.concat`, `string.format`, all three `gsub` branches, `gmatch`'s
  leading-`^` rewrite, `print`, host `push_bytes`/`push_string`, compiled
  literals). Checking only at intern time is too late - the dangerous `Vec`
  already exists by then.
- **Call dispatch.** A method or function call whose callee was not a plain name
  and which had a call as an argument dispatched the receiver instead of the
  method: `("ab"):find(id("b"))` raised "attempt to call a string value".
  `OP_MARK_CALL_BASE` was emitted before the callee was evaluated, so it had to
  guess how many values the callee had left on the stack, and the guess covered
  exactly two shapes. The marker now comes after, where the answer is invariant.
- **Cost accounting.** Budgets are enforced at the exact operation boundary
  rather than in batches of 64, so a script can no longer run dozens of ops past
  an exhausted budget; pending cost flushes before any reentrant call.
  `table.move` and `table.insert` charge per element instead of a flat 1
  (`table.move({}, 1, 2^30, 1)` was ~10^9 operations for cost 1). `table.sort`
  charges before sorting, not after. `consume_cost` saturates instead of wrapping
  a `u64` charge through `i64` and *raising* the budget.
- **Table operations.** `table.insert` at a position used `shift_remove` +
  `insert` per element, making one `table.insert(t, 1, v)` on a 50k array roughly
  10^9 memmoves, and left `pairs` order reversed; it now carries a value forward
  in place. `table.sort`'s default comparator silently accepted incomparable
  operands where reference raises a type error.
- **Multi-value rules.** A tail `...` now expands in non-local assignment and
  generic-`for` setup. Table constructors follow Lua's rules - only the final
  list field expands (`#{7, f()}` was 2, now 4) - via a `NewTableTracked` opcode
  recording the table's exact stack base, fixing crashes on nested constructors.
  Call boundaries enforce the 255 argument/result ceiling instead of wrapping a
  `u8` count.
- **Closures.** The compiler omitted `CloseUpvalues` when leaving a `do`/`if`
  block, at each `for` iteration, and on `break`, so a captured local's slot
  could be reused and observed with a later value.
- **Errors and tracebacks.** Errors carried no position, rendering `0:0:
  internal error: oops` where reference gives `input:1: oops`. Only `OP_CALL`
  refreshed `CallInfo.ip`, so tracebacks blamed the wrong caller line for errors
  inside iterators and metamethods. Multi-line calls were attributed to their
  closing line. `line_info` drifted out of alignment with `code` on essentially
  any real script, corrupting stack traces and the serialized line table.
- **Pattern conformance.** `matchdepth` leaked a level on six tail-call
  continuations and never reset between anchor positions, so a 250-byte subject
  died on its first `find`. `%f` treated slice-start as string-start, so
  `("abcd"):gsub("%f[%w]%w", "X")` gave `"XXXX"` instead of `"Xbcd"`. Position
  captures returned an empty string or panicked; escaped uppercase classes
  matched the wrong character (`%E` matched `"e"`); `%s` missed vertical tab;
  patterns ending in `%%` were rejected; character classes now close with
  reference's do-while, so `[]]` contains `]`. `gsub` no longer re-anchors a
  leading `^` to every remainder, and `gmatch` treats it as a literal caret.
- **Stdlib validation.** Optional arguments treat an explicit `nil` as absent
  like both references, so `("a.b"):find(".", nil, true)` no longer raises.
  `tonumber` parses the Lua number grammar including hex floats at the extremes
  (bit-exact against lua5.2/5.4), `getmetatable`/`setmetatable` honor
  `__metatable`, and `pairs` uses the builtin `next` so rebinding global `next`
  no longer breaks it. `string.format` is rewritten to Lua 5.4's error contract
  and full conversion set with reference-exact payloads.
- **Lexer conformance.** `skip_comment` recognised only `--[[`, so a `--[=[`
  body was lexed as live source - silent execution when the body was valid Lua.
  Bare CR was mishandled throughout. String literals now decode `\ddd`, `\xXX`
  and `\z`, and unknown escapes are a syntax error.
- **Semantics vs 5.4.** `for` control expressions evaluate in the enclosing
  scope, so `local i = 5; for i = i, 7 do` prints `5 6 7`. NaN ordered
  comparisons return false (`partial_cmp`'s `None` mapped to `Equal`, so `<=` and
  `>=` both returned true). `1 % math.huge` is `1.0`, `math.modf(inf)` has a zero
  fractional part.
- **Miscellany.** `_G` no longer stringifies keys, so `_G[1]` and `_G["1"]` are
  distinct. `Val::RustFn` has real value identity - `print == print` was false
  and host functions were unusable as table keys. A string `__index` handler
  chains instead of raising. Interned strings count toward the GC threshold, so a
  string-only workload no longer grows the heap unbounded. An explicit
  `gc_collect()` no longer re-enables automatic GC after `gc_disable_auto()`.
  `with_restricted_env` restores globals even if the closure panics. `on_error`
  fires for a host-direct `RustFunc` failure. The CLI rejects an invalid
  `--limit` instead of silently running unbudgeted.

### Performance

- `gmatch` no longer recopies its subject or recompiles its pattern every
  iteration, so a full scan is linear rather than quadratic:
  `examples/strings/patterns.lua` went 160ms to 60ms, more than absorbing the new
  per-primitive charging. Compiled patterns are cached under a bound on both
  entry count and total bytes, cleared on collection - without the bound a script
  using many distinct patterns grew host memory without limit.
- Closure upvalue lists are shared rather than cloned per call, literals are
  interned and inline caches shared once per `(State, Bytecode)`, global writes
  are cached per site, `pairs` iterates through a validated cursor, return values
  slide over the dead frame instead of round-tripping a `Vec`, and finalize
  strips dead instructions. Against 0.3.0 on the same host, `calls/many_literals`
  went 574ms to 57ms, `parse/large_source` 31ms to 8ms, `globals/write` 112ms to
  32ms and `fields/miss` 149ms to 77ms. `fields/same_obj_read` regressed 112ms to
  169ms; this is known and accepted for this release. See the README table for
  the full set.

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
