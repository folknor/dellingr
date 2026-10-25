# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

Saves and replays from 0.5.0 do not carry over: snapshot `FORMAT_VERSION` is
13 (was 7), with no migration, and `COST_MODEL_VERSION` is 8 (was 5).
Re-measure tick budgets and re-record replays.

### Changed (breaking): execution

- Register VM. The compiler builds a syntax tree per function and emits 64-bit
  register bytecode in place of the 32-bit stack bytecode. Results, evaluation
  order, `pairs` order, rendered values, RNG draws and `cost_used` are
  unchanged for every run that never reaches `MAX_STACK_SIZE`.
  - A Lua call reserves its whole register frame plus four slots of headroom
    when admitted, so a run that reaches the cap fails at the call that cannot
    reserve its frame, four slots earlier than before; in return an admitted
    function's own instructions never fail the cap. Runs that reach the cap
    end in a different state.
  - A register keeps its last value reachable until overwritten or returned,
    which can move when a collection reclaims it.
  - `JumpTooFar` now means a span of 2^23 instructions (was 2^15);
    `limits::MAX_VERIFIER_MARKER_DEPTH` is removed.
- Lua-to-Lua calls no longer recurse on the native stack, so `MAX_CALL_DEPTH`
  needs no native stack per level. Calls made from Rust (host `call`,
  metamethod handlers, generic-`for` iterators, `table.sort` comparators,
  `gsub` replacement functions) are bounded by the new
  `limits::MAX_NATIVE_CALL_DEPTH` (150) and raise
  `ErrorKind::NativeCallDepthExceeded`. A script recursing deeper than 150
  through such calls now fails there.
- The native-stack envelope is 2 MiB in release builds and 4 MiB in debug
  builds (was 2 MiB for both). A debug-build host running scripts on its own
  threads should give them 4 MiB. See docs/limits.md, "Envelope".
- `math.sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `exp`, `log` and
  `^` are in-crate ports of CORE-MATH's correctly rounded functions instead of
  the platform libm: results are the same on every host, and can differ in the
  last place from before and from reference Lua. Building for 32-bit x86
  without SSE2 is a compile error.
- The floating-point environment is enforced: host calls into a `State`,
  `Program::compile`, `save_state` and `load_state` refuse flush-to-zero,
  denormals-are-zero, directed rounding and unmasked exceptions with
  `ErrorKind::UnsupportedFloatEnvironment` (and its `SaveError`/`LoadError`
  twins), carrying a decoded `FloatEnvironment`. A callback that returns with
  the environment changed fails at its return. See docs/determinism.md.
- `_G` is the global table itself, not a proxy. `pairs(_G)` yields the globals
  in binding order, `rawget`/`rawset` on `_G` read and define globals, and
  `with_restricted_env` swaps the table's contents. `State::raw` has a global
  table too, without a `_G` binding. `getmetatable(_G)` stays `false`.
- `__index` and `__newindex` count metamethod depth by one rule: a handler
  table that answers from its own contents takes no unit, a table continuing
  to its own handler takes one, a function handler takes one (docs/limits.md).
  Results at the depth limit no longer depend on the lookup site's history or
  spelling. Chains of tables need no native stack or value-stack slot per
  step.
- A panic unwinding through a State's execution poisons it: later fallible
  methods return `ErrorKind::StatePoisoned` (and its `SaveError`/`LoadError`
  twins), `gc_collect` does nothing, and `State::is_poisoned` reports it.
- A `RustFunc` returning `Ok(n)` with fewer than `n` values fails with
  `ErrorKind::ResultCountExceedsStack` instead of padding with nil.
- `table.insert` and `table.remove` through sequence metamethods refuse a
  shift of `limits::TABLE_SORT_MAX_LEN` elements or more
  (`LibraryError::SequenceShiftTooLarge`).
- Any variable use of `_ENV` is a syntax error
  (`SyntaxError::UnsupportedEnvironmentName`).
- An assignment takes at most `limits::MAX_ASSIGNMENT_TARGETS` (255) targets
  (`TooManyAssignmentTargets`), and a function needing more registers than
  bytecode can name fails with `TooManyRegisters`.

### Changed (breaking): replay-visible details

- Cost: `rawset` charges 1, like a table write; `string.gsub` rejects an invalid
  replacement before charging; a leading-`^` `gmatch` pattern charges one unit
  less.
- Lines: operators record their operator's line, `function` statements and
  expressions their `function` keyword's, and `...` its own; other
  instructions take the last token consumed. Errors raised through a `gmatch`
  iterator are located at the calling line. Error locations, tracebacks and
  snapshot bytes change accordingly.
- Identities and collection timing: metamethod event names are interned once
  per State; registers keep values alive until overwritten; a cleared hash key
  stays alive, and a valid `next` control, until its slot compacts; a dropped
  `Anchor` releases its value. Each can move collections and with them
  `%p`/`tostring` ids.
- Global `unpack` and `table.unpack` are distinct functions in every build
  (release LTO used to merge them), and one host function registered under two
  ids is two functions.
- Invalid UTF-8 in printed text and error values reaches the host as bytes, not
  U+FFFD.
- `x ^ y` with `|y| >= 2^1014` returns the overflowed or underflowed result
  directly.
- A `gmatch` iterator whose result push is refused no longer skips the match.
- Snapshot contents: the global table is saved whole with its pins and rebuilt
  in saved order; tables save their logical array length and live window plus
  their hash-slot history; `%p` ids of string literals, environment objects
  and setup-rendered values survive a load; string-literal ids follow the
  save's function order, so saves are byte-stable; functions record their
  definition line; bytecode drops unneeded upvalue closes.

### Changed (breaking): host API

- One compile path: `Program::compile(source, name)` takes bytes for both and
  is the only way to compile. `Engine` is removed (`new_state` becomes
  `State::new`/`with_callbacks`, `raw` becomes `State::raw`, and
  `State::raw_with_callbacks` is new); `load_string`/`load_string_named` are
  removed (compile, then `State::load`); `analyze_cost` becomes
  `Program::analyze_cost`; `State::empty` is `State::raw`.
- Chunk names are bytes: `Program::source_name`, `Error::source_name` and
  `State::current_source` return `Option<&[u8]>`. `HostCallbacks::on_print`
  receives `source: Option<&[u8]>` and `message: &[u8]` exactly, and has no
  default body. `HostCallbacks::on_error` takes only the error.
  `StackFrame::source` is `Option<Arc<[u8]>>` and function names are
  `Option<Arc<str>>`. Source names over `limits::MAX_SOURCE_NAME_BYTES` are
  rejected; long diagnostic function names are truncated.
- Rust functions are State-owned table entries, never code addresses.
  `add_rust_fn` and `register_rust_fn` return a State-bound `RustFnHandle`
  that `push_rust_fn` takes; `push_named_rust_fn` is removed;
  `registered_rust_fn` looks a handle up by id. Registering an id the State
  already has always fails.
- `register_rust_fn` and `set_global_named_rust_fn` return the crate's
  `Result`: a duplicate id is `ErrorKind::DuplicateFunctionRegistration`, an
  over-long id the new `FunctionIdSizeExceeded`, a full table
  `RustFunctionLimit`.
- `Anchor` is an owned handle: not `Copy`/`Clone`, released on drop;
  `release_anchor` consumes it, and `push_anchor`, `call_anchor` and
  `anchor_type` take `&Anchor`.
- `State::with_restricted_env` returns `Result<R>`.
- `State::bytes_with_tostring_meta` is renamed `tostring_bytes`.
- `State::gc_set_threshold` installs a fixed threshold; `gc_set_auto_threshold`
  resumes adaptive collection.
- `State::open_libs` inside `with_restricted_env` is refused
  (`ErrorKind::OpenLibsInRestrictedEnv`).
- `State::gmatch_pattern_compilations` is removed.
- `ScopeCost` category counts are methods.
- The root re-export `MAX_STRING_BYTES` and `error::MAX_RESULTS` are removed;
  use `dellingr::limits`. `limits::GMATCH_PATTERN_CACHE_*` are renamed
  `limits::PATTERN_CACHE_*`.
- The `debug_parser`, `debug_vm` and `debug_gc` cargo features are removed.

### Changed (breaking): errors and diagnostics

- Errors are typed throughout; no error carries text describing what went
  wrong.
  - `ErrorKind::ScriptError { value: ErrorValue, prefix }` carries the raised
    value (exact string bytes, or a table's or function's rendering).
  - Library failures are `ErrorKind::LibraryError(LibraryError)`;
    `RuntimeError(String)` is gone. `ErrorKind::HostError(Box<dyn Error>)` is a
    `RustFunc`'s own failure, and `Error` implements `std::error::Error`.
  - `ErrorKind::InternalError` always means a dellingr bug; `ArgCount::Dynamic`
    passed to `call` is `DynamicArgumentsUnsupported`.
  - Snapshot errors live in the new `dellingr::snapshot` module with typed
    reasons; `LoadError` is `Setup`, `StatePoisoned` or
    `Snapshot(SnapshotError)`. `CorruptArena` names its location; other load
    messages are unchanged.
  - `Error` and `ArgError` fields are private, read through accessors, and the
    error enums are `#[non_exhaustive]`.
  - Removed: `ErrorKind::InvalidJump`, `ArgDetail::Custom` and `OutOfBounds`,
    `SyntaxError::UnexpectedTok` and `AssignmentOffsetTooLarge`,
    `LibraryFunction::Other`.
- `error(v)` adds the `chunk:line:` prefix only to strings, as Lua 5.4 does,
  and `error()` raises nil.
- Method calls number arguments as written: `s:sub(1.5)` is `bad argument #1`,
  and a bad receiver is `calling 'sub' on bad self`.
- Syntax errors use reference Lua's wording: `unexpected symbol near ')'`,
  `syntax error near '='`, `'=' or 'in' expected`, `function arguments
  expected`. No program changes between accepted and rejected.
- Tracebacks name an unnamed function `function <source:line>`;
  `StackFrame::defined_line` is new.
- `table.move` limit errors are argument errors (`bad argument #3 to 'move'`).
- A non-numeric numeric-`for` control and a bad metatable raise their own
  `TypeError` variants.
- A call whose callee frame cannot be reserved reports `StackOverflow` at the
  caller.
- `Error`'s `Display` no longer ends with a newline.

### Added

- `State::seal_globals` and `globals_sealed`: after sealing, globals set to nil
  are reclaimed, so computed names written through `_G` stay bounded. Saves
  record the phase and pins (docs/save-state.md).
- `Program::disassemble` and the CLI flag `--dump-bytecode`.
- Per-State GC policy: `State::with_gc_policy`,
  `with_callbacks_and_gc_policy`, `gc_set_policy`, `gc_set_auto_threshold`.
- `State::try_to_boolean`, `try_typ` and `try_raw_equal` report an invalid
  stack index.
- Snapshot byte limits and reference-pinned library limits are public in
  `limits`.
- `Error::library_function` and `Error::pattern_function` name the failing
  library function.
- The CLI reads the script from stdin when the input is `-`.

### Changed

- The `hotpath` profiler is an optional dependency, so a default build no
  longer compiles it.
- The published package ships the runtime sources, docs, README, license and
  lockfile only.
- `string.gsub`'s bad capture names it: `invalid capture index %2`.
- `--analyze` and `CostAnalysis::caveat()` say that native library charges are
  left out, and label a scope with no static cost `cost 0 (static)`.

### Fixed

- `on_error` fires for every execution error before host code sees it, also
  inside a Rust callback that handles the error itself.
- A table used as a FIFO no longer grows with its history, and an array
  cleared in any order releases its storage.
- Inline caches could return or overwrite the wrong field after billions of
  reuses of one heap slot.
- Array growth or a `table.sort` copy that cannot be reserved raises
  `ErrorKind::TableGrowthFailed` instead of panicking or aborting.
- An explicit nil optional argument takes its default (`error(msg, nil)`,
  `math.atan(y, nil)`, `math.log(x, nil)`).
- `table.concat` accepts a number separator.
- `string.gsub` checks subject and pattern before the replacement.
- `table.move` charges identically with and without a budget, and accepts an
  exact singleton key above 2^53.
- A reachable `ipairs` iterator can be saved.
- Host `get_table` keeps its key when an `__index` handler is refused before
  it runs; a refused `...` leaves live registers untouched.
- An unfinished short string reports the end of the line it broke off on.
- Snapshot decoding caps reservations by input size, and rejects impossible
  cost counters and malformed `gmatch` state.
- The CLI exits quietly when stdout is a closed pipe.

## [0.5.0] - 2026-10-14

Saves and replays from 0.4.0 do not carry over. Re-measure tick budgets and
re-record replays.

### Changed (breaking)

- `COST_MODEL_VERSION` is 5. Literal arithmetic is constant-folded and free,
  except `^`. `string.gmatch` charges pattern compilation once per iterator.
  `cost_used` includes cost pending when a runtime error ends a frame.
- Snapshot `FORMAT_VERSION` is 7; older saves are rejected. Loading also
  rejects duplicate globals, duplicate template keys, non-table metatables and
  out-of-range pointer ids.
- Tables have an array part. `pairs` visits keys `1..n` first, then the rest in
  insertion order. Constructors keep nil slots: `#{1, nil, 3}` is 3.
- Numbers print with `%.14g`: `0.1 + 0.2` is `0.3`, `1e15` is `1e+15`, NaN is
  `nan`, negative zero is `-0`. `%q` prints integral numbers in decimal.
- Snapshots save every NaN as one canonical value.
- `math.random` uses rejection sampling; the stream differs at rejected draws.
- `string.gsub` and `string.gmatch` follow Lua 5.4's empty-match rule.
- A field miss on a plain table returns nil. `t:insert(...)` no longer reaches
  `table.insert`.
- Strings share a metatable. `getmetatable("")` returns it; methods resolve
  through its `__index`, not the global `string`. Its `__call` and
  `__tostring` are honoured.
- Rust functions render as `function: 0x<id>`, stable across save and load.
- Stdlib integer arguments reject non-integral numbers instead of truncating.
- `getmetatable(_G)` returns `false`; `setmetatable(_G, ...)` errors.
- Bad-argument messages name the function: `bad argument #1 to 'sin' (...)`.
- Host API:
  - `State::set_global` returns `Result<()>`.
  - `State::call` rejects `ArgCount::Dynamic`.
  - `State::cost_remaining()` returns `i64::MAX` with no budget set.
  - `State::table_len` and `State::table_sort` are crate-internal; use
    `State::raw_len` and call `table.sort` through the stack.
  - `State::table_remove_at` rejects positions outside `table.remove`'s range.
  - `State::to_string` and `to_string_with_meta` reject output over
    `MAX_STRING_BYTES`; `to_string_with_meta` honours a string `__tostring`.
  - The `load_state` setup closure returns `Result<(), E>`, surfaced as
    `LoadError::Setup(E)`.
  - `save_state` fails with `NotQuiescent` inside a Rust callback.
  - `HostCallbacks` boxes drop `+ Send`.
  - `ArgError` has a required `detail: ArgDetail` field.
- Error types: new `SourceSizeExceeded`, `PatternError`,
  `DuplicateFunctionRegistration`, `TooManyArguments`, `TooManyResults` and
  `SyntaxError::UnfinishedLongComment`. `SyntaxError::LParenLineStart` is
  removed. Limit errors carry `limit`; version errors carry `found` and
  `expected`; `CorruptArena` carries `reason`; `SaveError` gains `Internal`
  and `TooLarge`.
- `on_error` fires once for every error after a call dispatches. Host misuse
  rejected before dispatch returns an error without notifying.
- `State::current_source()` and the `source` passed to `on_print` / `on_error`
  name the running frame's chunk.
- A `(` on a new line continues the previous expression as a call.
- Pattern errors are raised only when matching reaches the malformed part.
- MSRV is Rust 1.99.
- CLI: `Cost used` and the `--analyze` caveat go to stderr; unknown options,
  extra input files and `--limit` with `--analyze` are usage errors.

### Added

- `State::raw_len` and `State::bytes_with_tostring_meta`.
- `dellingr::limits` exposes the runtime, compiler and pattern-cache caps.
- `string.gmatch` accepts Lua 5.4's `init` argument.

### Changed

- The table library, `table.sort` included, honours `__len`, `__index` and
  `__newindex` as in Lua 5.4. `table.sort` reads all elements before writing
  any back, so handlers see a different call order than in reference Lua.
- `elseif` and expression suffix chains no longer count against the
  syntax-depth limit.
- `analyze_cost` reports `Call instructions` per scope.

### Fixed

- `break` inside a nested function no longer binds to the enclosing loop.
- Unterminated long comments are an error, not a silent end of file.
- CR LF CR counts as two line breaks.
- Cyclic `__call` chains no longer abort the process.
- `open_libs` with a conflicting function registration changes nothing.
- `_G` keys are byte-exact; non-UTF-8 keys no longer alias.
- Assigning nil to an unknown global creates no entry.
- Forged snapshot bytecode can no longer underflow a table's tombstone count.
- Host table methods keep their operands when they reject a call, and return
  `InvalidStackIndex` instead of panicking on bad indices.
- `State::get_table` on a string follows the string metatable.
- `State::call_anchor` validates its argument count before touching the stack.
- `print` and `error` reject oversized lossy UTF-8 output before building it.
- `tostring` keeps non-UTF-8 bytes returned by `__tostring`.
- `#` passes two operands to `__len`.
- Table-library ranges beyond +/-2^53 are rejected.
- `table.remove` on an empty sequence returns and clears `t[0]`.
- `table.concat` reports the reference invalid-value message.
- `string.find` no longer treats patterns containing `)` as plain text.
- `tonumber(s, base)` trims vertical tab.
- The upvalue pool and `%p` id map no longer grow without bound.
- Hash maps use a fixed seed instead of the OS-seeded `RandomState`.

### Performance

- `table.remove(t, 1)` and the `table.sort` writeback are linear, not
  quadratic.
- Re-adding a deleted hash key is O(1).
- Snapshot environment-delta encoding is no longer quadratic.
- Conditions compile to fused compare-and-branch opcodes.
- foldhash replaces SipHash on the table hash part.

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
  with no branch on atom kind. Compilation is infallible; malformed pattern
  parts raise from `matches_bytes_from` only when reached, and unfinished
  captures are rejected when a library function reads them.
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
