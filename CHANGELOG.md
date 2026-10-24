# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Testing

- The integration tests are one crate, `tests/integration`, instead of one
  binary per file: each former file is a module, grouped by subject (`cli`,
  `compiler`, `corpus`, `cost`, `failures`, `host`, `library`, `runtime`,
  `snapshot`), so a focused run can select a subject by module path. The
  snapshot suites are gated by a `cfg` on their module instead of compiling to
  empty binaries in the default sweep. `tests/native_stack.rs` stays its own
  target. A shared `support` module replaces the per-file compile-load-call
  helpers and error and print recorders, and a convention test keeps the tree
  to its named targets, every source reachable from `main.rs`, no dead-code
  allowances, and process-wide state marked.
- A small `tests/libm_symbols.rs` target checks the fat-LTO release CLI for
  platform libm transcendental symbols in both release sweeps, replacing the
  manual `scripts/check_libm_symbols.sh`.
- Calls that can re-enter Lua are wrapped in a `native_call!` marker macro
  instead of carrying a marker comment. It expands to the bare call; the
  debug-only `native-call-trace` cargo feature records each executed edge, and
  a curated check runs every native-stack scenario under it to confirm the
  inventory's paths by execution.
- Every `examples/` file that defines a global `_bench` prints its computed
  `_bench()` result in its success line, so the differential gate compares a
  value against lua5.2/lua5.4 instead of a constant string; a corpus test
  enforces the footer shape. Each registered workload pair's kernel and shared
  helpers must now match in bytecode and literal pools as well as in cost
  fingerprint, and every registered `hotpath_file` opens with a `Probe:`,
  `Historical probe:` or `Workload:` header. The `benchmark` verdict workload
  now repeats the composite kernel of `examples/composite/benchmark.lua`
  exactly, so its stored results before the re-pin do not compare.

### Changed (breaking)

- Register VM. The compiler builds a syntax tree per function and generates
  64-bit register bytecode from it (locals live in registers, constant
  operands are separate opcodes, comparisons branch in one instruction), in
  place of the 32-bit stack bytecode. Results, evaluation and store order,
  `pairs` order, rendered values, RNG draws, error kinds and lines, and
  `cost_used` are unchanged for every run that never reaches
  `MAX_STACK_SIZE`. The differences:
  - **Replay-visible at the stack cap:** a Lua call reserves its function's
    whole register frame when it is admitted, so a run that reaches
    `MAX_STACK_SIZE` now fails at the call that cannot reserve its frame,
    before the body runs. The surviving state, RNG position, error location,
    traceback and `cost_used` of such runs differ. This requires a
    `COST_MODEL_VERSION` bump.
  - Multiple-assignment nil padding no longer occupies the stack, and a
    fixed number of results returns into registers the caller already
    holds, so some assignments and calls that failed with `StackOverflow`
    near the cap now succeed.
  - **Replay-visible at the stack cap:** admission also reserves four free
    slots above the frame, the largest call window an instruction opens, so
    a call (from Lua or the host) is refused four slots earlier. In return
    an admitted function's own instructions never fail the cap: closures,
    template constructors (`{x = 1}`), concatenation, generic `for` result
    delivery and every table read, store and length that calls no handler
    work in the registers. Near the cap these used to raise `StackOverflow`
    depending on spelling (`obj.k` answered where `obj["k"]`, a string
    receiver's method or `t.newkey = v` raised), on `__index`/`__newindex`
    chain length, and on whether a loop had more variables than its Rust
    iterator returned. A handler call can still be refused there, by the
    handler's own admission.
  - `__newindex` table chains and `__index` chains through strings are
    resolved in a loop, without native recursion per step; their
    metamethod-depth units are unchanged.
  - A register whose value is no longer used keeps that value reachable
    until the function overwrites it or returns, which can move when a
    collection reclaims it (and so `%p` and `tostring` ids of values rebuilt
    later, and `object_count`).
  - Jumps carry 24-bit offsets: `JumpTooFar` now means a span of 2^23
    instructions or more, where it was 2^15.
  - Snapshot `FORMAT_VERSION` was bumped: instruction words are 64-bit and each
    function records its register count. Older saves are rejected.
  - `limits::MAX_VERIFIER_MARKER_DEPTH` is removed: the verifier no longer
    simulates an operand stack.
  - `--dump-bytecode` prints the register instruction set and each
    function's register count.

- Native-stack envelope split by build. Every runtime recursion limit at
  once is now promised to fit a 2 MiB thread in release builds and a 4 MiB
  thread in debug builds, where it was 2 MiB for both. A debug build of a
  host that runs scripts on threads it spawns with the default 2 MiB should
  give them 4 MiB (`std::thread::Builder::stack_size`); otherwise a deeply
  recursive script can abort the process instead of failing with a limit
  error. Parsing at the syntax budget still fits 2 MiB in both. The limits
  themselves are unchanged. See docs/limits.md, "Envelope".

- One compile path, byte-exact. `Program::compile(source, name)` is the only
  way to compile Lua: `source` is `impl AsRef<[u8]>` and `name` is
  `Option<&[u8]>`, so non-UTF-8 sources load (string literals keep their
  bytes) and chunk names are kept exactly. `Engine` is removed (its
  `compile`/`compile_named` become `Program::compile`, `new_state` becomes
  `State::new`/`State::with_callbacks`, `raw` becomes `State::raw`/the new
  `State::raw_with_callbacks`); `State::load_string` and
  `load_string_named` are removed (compile, then `State::load`); the free
  `analyze_cost(source)` and `Engine::analyze_cost` become
  `Program::analyze_cost(&self)`. `State::empty` is renamed `State::raw`.
  An oversized chunk name is still rejected before the source is examined.
- Chunk names are bytes: `Program::source_name`, `Error::source_name` and
  `State::current_source` return `Option<&[u8]>`, and `StackFrame::source`
  is `Option<Arc<[u8]>>`. `Display` renders them lossily. Snapshots store
  them as the same length-prefixed bytes as before, now without requiring
  UTF-8, so the format and existing saves are unchanged.
- `HostCallbacks::on_print(&mut self, source: Option<&[u8]>, line: u32,
  message: &[u8])`: the host receives the printed bytes and chunk name
  exactly. `DefaultCallbacks` and the CLI write the bytes unchanged.
- Typed errors. `ErrorKind::ScriptError { value: ErrorValue, prefix }`
  replaces `message: String`: `ErrorValue` is `Nil`, `Boolean`, `Number`,
  `String(Vec<u8>)` (exact bytes) or `Object { value_type, rendering }` for
  tables and functions, whose rendering is their `tostring` bytes.
  `error()` with no argument raises `Nil`, as `error(nil)` does, instead of
  the invented message "(error raised with no message)"; it renders as
  `nil`. Library failures are
  `ErrorKind::LibraryError(LibraryError)` instead of a rendered
  `RuntimeError(String)` plus `Error::library_error()`, which is removed.
  `RuntimeError(String)` is gone; `ErrorKind::HostError(Box<dyn
  std::error::Error + Send + Sync>)` is the kind for a `RustFunc`'s own
  failure (`"text".into()` builds one), recoverable through
  `Error::source` and the now-public `Error::into_kind`; `Error` implements
  `std::error::Error`. `ErrorKind::InternalError` wraps an opaque
  `InternalError` that only dellingr constructs, so it always means a
  dellingr bug; a host passing `ArgCount::Dynamic` to `call` or
  `call_anchor`, previously reported as an internal error, is the new
  `ErrorKind::DynamicArgumentsUnsupported { api: HostCallApi }`, whose text
  drops the `internal error: ` prefix. `SyntaxError::ExpectedToken` holds
  `expected: TokenKind` (the new public token vocabulary) and `found:
  Vec<u8>` (the token's source bytes); the never-constructed
  `SyntaxError::UnexpectedTok` is removed; a stray non-UTF-8 byte in source
  is the new `SyntaxError::InvalidByte`. `LibraryError`'s format
  `directive` fields are `Vec<u8>`.
- `State::bytes_with_tostring_meta` is renamed `State::tostring_bytes`.
- **Replay-visible:** invalid UTF-8 in printed text and in error values now
  reaches the host as the script's bytes instead of U+FFFD. `print` and
  `error` no longer check a threefold lossy expansion against the string
  cap: a `print` whose raw output fits the cap but whose U+FFFD expansion did
  not now prints, and such an `error` now raises its value instead of
  `StringSizeExceeded`. The CLI's `print` output is byte-exact, as reference
  Lua's is. Costs are unchanged.
- Error text changes from the byte-exact boundary are limited to: `error()`
  renders `nil` (above); `ArgCount::Dynamic` refusals lose the `internal
  error: ` prefix; and the CLI's `--analyze` names the chunk (file path or
  `stdin`) in a parse error, as running and `--dump-bytecode` already did,
  because it compiles through the same `Program::compile` call. The
  corrected diagnostics below and the typed-reasons entry list the rest.
- Diagnostics that mislead are corrected. Costs are unchanged.
  - Method calls number arguments as the script wrote them, as reference Lua
    does: `s:sub(1.5)` is now `bad argument #1 to 'sub' (number has no integer
    representation)` (was `#2`), and a bad receiver, as in
    `local t = {sub = string.sub}; t:sub(1)`, is now `calling 'sub' on bad
    self (string expected, got table)` (was `bad argument #1 to 'sub' (string
    expected, got table)`). This applies to every library function called
    with `:`. The call instruction carries the mark.
    `ArgError::arg_number` still reports the callee's own position;
    `ArgError::is_method_call` says whether the rendering shifted it.
  - Tracebacks call only the main chunk `main chunk`. An unnamed function is
    now `in function <source:line>` with the line it was defined on (was `in
    main chunk`). `StackFrame` gains `defined_line` (0 for a main chunk).
    Snapshots store each function's definition line (a u32 beside its name,
    still format version 10), so a restored function's traceback labels match
    the original run.
  - `table.move`'s `too many elements to move` is now `bad argument #3 to
    'move' (too many elements to move)`. It is `ErrorKind::ArgError` with
    the new `ArgDetail::TooManyElementsToMove`; `LibraryError::TooManyMoveElements`
    is removed. Likewise `destination wrap around` is now `bad argument #4
    to 'move' (destination wrap around)`, as in Lua 5.4: `ArgError` with the
    new `ArgDetail::DestinationWrapAround`, and
    `LibraryError::MoveDestinationWrapAround` is removed.
  - `error(v)` adds the `chunk:line:` position only to a string value, as Lua
    5.4 does; `error()` records `PrefixLocation::Suppressed` for any other
    value. Before and after: `error(42)` was `t:1: 42`, now `42`;
    `error(true)` was `t:1: true`, now `true`; `error(nil)` was `t:1: nil`,
    now `nil`, and so is `error()` (see above); `error({})` was `t:1: table: 0x1`, now
    `table: 0x1` (likewise functions and `__tostring` renderings). Strings
    are unchanged. Lua 5.2 still prefixes numbers; docs/compatibility.md
    records the choice.
  - Syntax errors use reference Lua's wording instead of naming a token the
    grammar never required. New `SyntaxError` variants `UnexpectedSymbol`,
    `MalformedStatement`, `ForSeparatorExpected` and
    `FunctionArgumentsExpected`, each with the offending token's `found`
    bytes. The statement parser now stops only at a block end (`end`,
    `else`, `elseif`, `until`, end of input), and an empty `return` only
    before one or `;`, as reference Lua does. Before and after (columns
    omitted):
    - `local x = )`: `'nil' expected near ')'` is now `unexpected symbol near
      ')'`; the same for any token that cannot start an expression.
    - `x = 1 )` and `= 1` at top level: `'<eof>' expected near ')'` / `near
      '='` are now `unexpected symbol near ...`; inside a block, `if x then )
      end`: `'end' expected near ')'` is now `unexpected symbol near ')'`.
    - `return )`: `'<eof>' expected near ')'` (or `'end' expected` in a
      block) is now `unexpected symbol near ')'`.
    - `a, ) = 1, 2` and `a, 1 = 1, 2`: `'<name>' expected near ...` is now
      `unexpected symbol near ...`.
    - `(x) = 1` and `a, f() = 1, 2`: `'=' expected near '='` is now `syntax
      error near '='`; `f() = 1`: `'<eof>' expected near '='` is now `syntax
      error near '='`; a bare expression such as `x;`, `x + 1` or `x do end`:
      `'=' expected near ...` is now `syntax error near ...`.
    - `for i do end`: `'=' expected near 'do'` is now `'=' or 'in' expected
      near 'do'`.
    - `a:b z`: `'(' expected near 'z'` is now `function arguments expected
      near 'z'`.
    - Expected names, literals and end of input are no longer quoted:
      `'<name>' expected near '1'` is now `<name> expected near '1'`, and
      `'<eof>' expected near 'end'` is now `<eof> expected near 'end'`.
    Where the parser reaches the end of the source needing a token, the
    error is still `unexpected <eof>` and still recoverable. No program that compiled before is rejected, and no
    rejected one now compiles.
- No error type carries text that describes what went wrong; every such
  reason is an enum a host can match on. Strings that remain are data
  (function names and ids, environment tokens, chunk names, rendered value
  paths).
  - `LibraryFunction::Other(String)` is removed (nothing constructed it);
    `LibraryFunction` is `Copy` and `Error::library_function` returns it by
    value.
  - `ErrorKind::DynamicArgumentsUnsupported::api` is a `HostCallApi` (`Call`,
    `CallAnchor`), rendering `call` / `call_anchor` as before.
  - `SyntaxError::ExpectedToken::expected` is an `error::TokenKind`.
  - Snapshot errors (`dellingr::snapshot`, a new module that also holds the
    six types still exported at the root): `SnapshotError::DecodeError` is
    `{ offset, reason: DecodeReason }` (an invalid bool or tag carries the
    byte); `CorruptArena::reason` is a `CorruptionReason` (out-of-range
    references, duplicates with the `KeyForm` that collided, nil and NaN
    keys, and `Table`, `Environment` and `Gmatch` families);
    `InvalidTable` is `{ location, step: TableRestoreStep, failure:
    TableRestoreFailure }`; `InvalidBytecode::reason` is an
    `InvalidBytecodeReason`, whose `Validation` variant carries the
    verifier's now-public `ValidationReason` (with `BytecodeOperand`,
    `BytecodePool` and `CacheFamily`); `FieldSizeExceeded::kind` becomes
    `field: SnapshotField`; `SaveError::Internal` wraps an opaque
    `SaveInternalError`; `SaveError::TooLarge` is `{ collection:
    SaveCollection, count, limit }`. Every load error renders its previous
    text.
  - A save-side byte field over its limit, including a function id over
    `MAX_SAVE_IDENTIFIER_BYTES` passed to `register_rust_fn`, is the new
    `SaveError::FieldSizeExceeded { field, size, limit }` instead of
    `TooLarge`, because its size is a byte length, not a count: `save function
    id count 4097 exceeds the supported limit` is now `save function id size
    4097 exceeds limit 4096`. A collection too long for its length prefix
    names itself instead of `vector`: `save vector count N exceeds the
    supported limit` is now, for example, `save table entries count N exceeds
    the supported limit`.

- Snapshot format version 10. Every bytecode inline-cache operand
  (`GET_GLOBAL`, `SET_GLOBAL`, `GET_FIELD`, `SET_FIELD` and the generic-for
  `pairs` cursor) now uses one encoding: biased by one, 0 meaning uncached;
  `GET_GLOBAL`, `GET_FIELD` and `SET_FIELD` used 255 for uncached before.
  Each saved function also stores its `pairs`-cursor count beside the other
  cache counts instead of the loader recomputing it from the code. Capacity
  is unchanged at 255 cached sites per cache family per function.
  **Replay-visible:** saves from earlier format versions are refused with
  `UnsupportedVersion`; there is no migration. Execution, costs and `pairs`
  order are unchanged.
- Cost model version 6: `string.gmatch` compiles a pattern's leading `^` as
  a literal byte instead of rewriting the pattern to `%^`, so such an
  iterator's first matching call charges the pattern's actual length, one
  unit less than before. **Replay-visible:** `cost_used` drops by one per such
  iterator, and a saved gmatch iterator stores its pattern as written (`^x`,
  not `%^x`). A leading-`^` gmatch pattern of exactly the maximum string size
  is no longer rejected at iterator creation; the rewrite made it one byte too
  long. Results are unchanged.
- Lua-to-Lua calls no longer recurse on the native stack: each dispatch-loop
  invocation runs over a frame stack of its own and continues in the callee,
  so `MAX_CALL_DEPTH` (1000) is reached without native stack per level in
  every build profile and on any thread. What still runs on the native stack
  is every call made from Rust - host `call`/`call_anchor`,
  `__index`/`__newindex`/`__len`/`__tostring` handlers, generic-for
  iterator functions, `table.sort` comparators, `string.gsub` replacement
  functions - and those are now bounded by the new
  `limits::MAX_NATIVE_CALL_DEPTH` (150; reference Lua's `LUAI_MAXCCALLS` is
  200), raising the new `ErrorKind::NativeCallDepthExceeded`. The value is
  what lets every recursion limit at once fit a 2 MiB thread in an
  unoptimized build; docs/limits.md, "Native stack", states the envelope.
  Recursion through such calls used to be bounded only by `MAX_CALL_DEPTH`
  (Lua frames) or not at all (a callback calling Rust functions through
  `State::call`).
  **Replay-visible:** a script that recursed between 150 and 1000 levels
  deep through such calls (a recursive `table.sort` comparator, a recursive
  `gsub` replacement function, a recursive iterator) now fails with
  `NativeCallDepthExceeded` at 150 nested calls from Rust, the host's own
  call included, instead of running or failing at `MAX_CALL_DEPTH`; a
  recursive `__index` or `__newindex` function stops there too, instead of
  at the `MetamethodDepthExceeded` it raised at 200.
- `Error` is one pointer wide (its fields live behind a `Box`), which shrinks
  every `Result` on the native stack; in the dev profile a Lua call level
  needed about 43 KB of native stack before and 6 KB after, before the call
  flattening above removed that cost altogether.
- `Anchor` is an owned handle: no longer `Copy` or `Clone`, and dropping it
  releases the anchored value (the State reclaims the slot at its next
  collection or anchor creation). `release_anchor(Anchor)` consumes the
  handle and releases at once; `push_anchor`, `call_anchor` and
  `anchor_type` take `&Anchor`. A released or stale handle can no longer
  exist, so `ErrorKind::InvalidAnchor` now means only a handle from another
  State, and its message says so. `anchor_count` counts live handles.
  **Replay-visible:** a value whose handle is dropped becomes collectable,
  where it used to stay rooted until `release_anchor`, so collections can
  free more, which can change rendered `tostring`/`%p` identities and
  snapshot contents. Drops on another thread take effect at whichever
  collection follows them; synchronize them when that boundary matters to a
  replay.
- Snapshot `FORMAT_VERSION` is 9; older saves are rejected. The payload
  records whether the globals were sealed. A table saves its logical array
  length (now 64-bit) and only the slots from its first live array key to
  its last, instead of every slot back to key 1, so two tables holding the
  same contents save the same bytes whatever their history.
  **Replay-visible (snapshot contents):** loading now rebuilds the globals in
  saved order instead of
  keeping the setup's order and appending saved-only names, so a State whose
  host set globals in a different order than its load setup now reloads to
  byte-identical saves.
- `LoadError<E>` now has three variants: `Setup(E)`, `StatePoisoned`, and
  `Snapshot(SnapshotError)`. Every other former `LoadError` variant moved,
  unchanged, to the new exported `SnapshotError` (so `LoadError::BadMagic`
  becomes `LoadError::Snapshot(SnapshotError::BadMagic)`);
  `LoadError::snapshot()` returns it, and `Error::source` exposes it. Display
  text is unchanged.
- `error::StackFrame::source` and `function_name`, and
  `error::VmFault::function_name`, are `Option<Arc<str>>` instead of
  `Option<String>`: they share the chunk's own allocations rather than
  copying the names per frame per error. `as_deref()` reads them as before.
- `limits::GMATCH_PATTERN_CACHE_ENTRIES` and `limits::GMATCH_PATTERN_CACHE_BYTES`
  are renamed `limits::PATTERN_CACHE_ENTRIES` and `limits::PATTERN_CACHE_BYTES`
  (same values): the per-State compiled-pattern cache they bound now also
  serves `string.find`, `string.match` and `string.gsub`, not only
  `string.gmatch`. Costs, results and errors are unchanged.
- A panic that unwinds through a State's execution (a panicking `RustFunc`
  or `on_error`, a panic inside `with_restricted_env`, or a VM-internal
  panic) now poisons the State. Every later `Result`-returning public method
  refuses with the new `ErrorKind::StatePoisoned` (`SaveError::StatePoisoned`
  from `save_state` and function registration, `LoadError::StatePoisoned`
  when a `load_state` setup closure returns success after catching a panic);
  `gc_collect` becomes a no-op and `State::is_poisoned` reports the flag.
  Reusing a State after catching a panic was never sound, despite the old
  `with_restricted_env` documentation. A callback that catches a panic and
  returns normally has its enclosing execution abandoned, and the host call
  returns `StatePoisoned`.
- `State::with_restricted_env` returns `Result<R>`, refusing a poisoned State
  without running the closure.
- A `RustFunc` that returns `Ok(n)` with fewer than `n` values in its frame
  now fails the call with the new `ErrorKind::ResultCountExceedsStack`
  instead of having the missing results padded with nil. This applies to
  host `State::call`, Lua calls and generic-`for` iterators alike. Callers
  that request more results than a callback returns are still padded with
  nil as before.
- `ErrorKind::InvalidJump` is removed. Verified bytecode execution reports
  internal dispatch failures as `InternalError`, with instruction context
  available through `Error::vm_fault()`.
- `LoadError::InvalidTable` reasons no longer carry a leading `0:0: `
  location prefix.
- `HostCallbacks::on_print` has no default body; every implementation must
  define it. `DefaultCallbacks` writes to stdout and drops write errors instead
  of panicking.
- `Error`'s `Display` no longer ends with a newline after the traceback.
- Snapshot loading rejects gmatch iterator state that lacks its cached pattern
  key, has a non-`true` compile flag, or an out-of-range `lastmatch`.
- `State::gc_set_threshold` installs a fixed threshold that collections no
  longer recompute. Use `gc_set_auto_threshold` to resume adaptive collection.
- Source names over `limits::MAX_SOURCE_NAME_BYTES` are rejected with
  `ErrorKind::SourceNameSizeExceeded` before parsing, and `register_rust_fn`
  rejects ids over `limits::MAX_SAVE_IDENTIFIER_BYTES` before changing
  either registry. Diagnostic function names longer than
  `limits::MAX_CHUNK_NAME_BYTES` are truncated, so such chunks still save.
- `Error` and `ArgError` fields are private. Read them through
  `Error::kind`, `stack_trace`, `line_num` and `column`, and
  `ArgError::arg_number`, `func_name`, `detail`, `expected` and `received`.
  `Error::new`, `line_num` and `column` use `u32` locations.
- `ArgDetail::Custom` and `ArgDetail::OutOfBounds` are removed, and
  `ArgDetail` equality compares variants exactly.
- `ErrorKind`, `ArgDetail`, `LibraryError`, `LibraryFunction`,
  `PatternErrorDetail`, `MalformedPatternDetail`, `SyntaxError`, `TypeError`,
  `SaveError` and `LoadError` are `#[non_exhaustive]`; matches on them outside
  the crate need a wildcard arm.
- Snapshot loading rejects cost counters the runtime cannot produce: an
  unbudgeted meter whose remaining or limit is not `i64::MAX`, or a budgeted
  one whose remaining is not its limit minus the cost used.
- `HostCallbacks::on_error` takes only the error; the reporting frame's source
  is available as `Error::source_name`.
- `ScopeCost` category counts are methods (`arithmetic_ops()`, `negations()`,
  `table_creations()`, `table_writes()`, `array_elements()`,
  `function_calls()`), so `ScopeCost` can no longer be built with a struct
  literal outside the crate.
- Binary operators, unary operators and concatenation record the line of
  their operator token, so runtime errors and tracebacks in multi-line
  expressions name the operator's line. Other instructions take the line of
  the last token consumed before they are emitted, where they previously took
  the line of their statement's first token or of the last token the parser
  explicitly expected. Bytecode line metadata, and with it snapshot bytes for
  multi-line chunks, changes accordingly.
- `LibraryError` gains `InvalidNextKey`, `PositionOutOfBounds`, `ArrayTooBig`,
  `TostringMustReturnString` and `FormatPointerIdentitySpaceExhausted`; those
  runtime errors keep their message text.
- `Error::library_function` answers for every typed library failure that
  names a function, not only `table.sort`'s default comparison: it is the new
  `LibraryError::function()` for an `ErrorKind::LibraryError`
  (`WrongNumberOfArguments`, `IntegerKeyRangeExceeded`,
  `ObjectLengthNotInteger`, `InvalidConcatValue` and `ArrayTooBig`), so the
  variant is its one source. `LibraryError::ObjectLengthNotInteger` gains a
  `function` field naming the caller of `__len` (`concat`, `unpack`,
  `insert`, `remove` or `sort`), and `LibraryFunction::Sort` now means
  `table.sort` in general. Message text is unchanged.
- The root re-export `dellingr::MAX_STRING_BYTES` is removed; use
  `dellingr::limits::MAX_STRING_BYTES`. Likewise `dellingr::error::MAX_RESULTS`
  is removed in favour of `dellingr::limits::MAX_RESULTS`.
- Snapshot loading reports a saved table whose entries the runtime table
  rejects as `LoadError::InvalidTable`, naming the table and the underlying
  failure, where it previously reported `LoadError::CorruptArena`.
- `LoadError::CorruptArena` gains a `location: TableLoadLocation` field naming
  the arena or nested collection its `index` refers to, and its message reads
  `corrupt save <location> at index ...` instead of `corrupt save arena at
  index ...`. `TableLoadLocation` gains `Strings`, `Objects`, `Bytecode`,
  `Upvalues`, `Globals`, `PointerIds`, `TableEntries`, `EnvironmentDeltas`,
  `EnvironmentDeleted`, `EnvironmentUpserts`, `EnvironmentOrder`,
  `EnvironmentArray` and `EnvironmentMetatable`. A non-object metatable value
  now reports its owner's index (the saved table's object index, or the
  environment delta's index) rather than 0.
- The `debug_parser`, `debug_vm` and `debug_gc` cargo features are removed.
  They printed straight to stdout, bypassing `HostCallbacks`. Use the CLI's
  `--dump-bytecode` or `Program::disassemble` to inspect compiled bytecode.
- **Replay-visible (cost):** `COST_MODEL_VERSION` is 8. `rawset` charges a
  flat 1, like a table-write opcode, where it was free, and `string.gsub`
  rejects an invalid replacement argument before charging for its subject
  and pattern.
- **Replay-visible (snapshot contents, GC timing):** snapshot
  `FORMAT_VERSION` is 12; older saves are rejected. A cleared hash key now
  stays a valid `next` control until insertion compacts its slot: the
  collector keeps a tombstone's key alive, and saves record each table's
  hash-slot history (dead keys, vacant positions and storage kind), for
  ordinary tables and environment-table deltas alike, so a traversal that
  clears keys as it goes resumes identically after a collection or a load.
  Keys of cleared entries can therefore outlive their last reference until
  compaction, which moves when collections reclaim them. In a loaded
  environment table, keys that setup added and the save lacks now follow the
  saved keys in `pairs` order.
- **Replay-visible (lines):** a `function` statement (`function f()`,
  `function t.f()`, `function t:m()`, `local function f()`) records the line
  of its `function` keyword for the closure and the store, and `...` records
  its own line, where they could take a later line such as that of the
  function's closing `end`. Error
  locations, tracebacks and snapshot bytes for multi-line declarations change.
- A Lua-to-Lua call whose callee frame cannot be reserved now reports its
  `StackOverflow` at the caller's call instruction; the never-entered callee
  no longer appears in the traceback.
- A non-numeric numeric `for` control raises the new
  `TypeError::NumericForInitialValue`, `NumericForLimit` or `NumericForStep`
  (`numeric for loop step must be a number, got nil`) instead of
  `TypeError::Arithmetic`.
- A host `set_metatable_of` with a metatable that is neither a table nor nil
  raises the new `TypeError::InvalidMetatable` ("metatable must be a table or
  nil") instead of `TypeError::TableIndex`.
- `State::open_libs` inside `State::with_restricted_env` is refused with the
  new `ErrorKind::OpenLibsInRestrictedEnv`, before any state changes.
- Every variable use of `_ENV` (read, assignment, local, parameter, loop
  variable, function name) is the new syntax error
  `SyntaxError::UnsupportedEnvironmentName` ("the special name '_ENV' is not
  supported"). Field names such as `t._ENV` are unaffected.
- `SyntaxError::AssignmentOffsetTooLarge` is removed; the assignment target
  cap rejects every list it could.
- `State::gmatch_pattern_compilations` is removed; the compiled-pattern
  counter is test-only instrumentation.
- **Replay-visible (lines):** an anonymous `function ... end` expression
  records its `function` keyword's line, as function statements do. Error
  locations, tracebacks and snapshot bytes change.
- **Replay-visible:** `table.insert` and `table.remove` through sequence
  metamethods refuse a shift of `limits::TABLE_SORT_MAX_LEN` elements or more
  with the new `LibraryError::SequenceShiftTooLarge` ("sequence shift too
  large in 'insert'"), before any handler runs. `LibraryError::function()`
  names `Insert` or `Remove`.
- **Replay-visible (snapshot contents):** the `%p` and `tostring` identities
  of environment objects, and of values reachable only from them, survive a
  load, also when a global shadows the builtin. Saves carry those ids.
- **Replay-visible (snapshot contents):** an environment-table delta saves its
  array part as the logical length plus the span from the first live array
  key to the last, as ordinary tables do, instead of every slot up to the
  logical length (`FORMAT_VERSION` stays 12). On load the saved shape is
  authoritative: nil slots setup left beyond it are no longer merged in, so a
  load whose setup differs from the saving State's can place later integer
  writes differently.
- **Replay-visible (lines):** an error raised through a `string.gmatch`
  iterator, such as a malformed pattern found on its first call, is located
  at the script line that called the iterator, and the internal shim frame is
  left out of the traceback. A user chunk named `string.gmatch` keeps its own
  locations, and an iterator restored from a snapshot is located the same way.
  The same holds for an error inside `string.gmatch` while it builds the
  iterator, such as budget exhaustion: it is located at the calling line, not
  `string.gmatch:<n>:`.
- `_G` is the global table itself, no longer an empty proxy whose handlers
  forwarded to a separate store. `pairs(_G)` and `next(_G)` enumerate the
  globals, `rawget(_G, "print")` is `print`, and `rawset(_G, "x", 1)` defines
  the global `x`; before, a raw write created a second `x` that `_G.x` read
  and `x` did not. Every key type is a global table key; non-UTF-8 and other
  non-string keys written inside `with_restricted_env` no longer outlive the
  restriction, which now swaps the table's contents, so every reference to
  `_G` sees the environment in force. `State::raw` has a global table too,
  without a `_G` binding. `getmetatable(_G)` stays `false` and
  `setmetatable(_G, ...)` still errors; the metatable no longer has
  `__index`/`__newindex` handlers. Reopening the libraries keeps the same
  `_G` object. Global access, `_G` field access and costs are otherwise
  unchanged.
  - **Replay-visible (`pairs` order):** `pairs(_G)` now yields the globals,
    in binding order (a cleared and redefined global keeps its place).
  - **Replay-visible (snapshot contents):** `FORMAT_VERSION` 13. The global
    table is saved whole with its pins, in place of the named bindings and
    the proxy's environment delta; older saves are refused. Load setup now
    contributes only string-keyed globals the save does not mention, and a
    sealed load keeps a pinned deletion even through a setup that no longer
    installs the name.
  - **Replay-visible (sealed globals):** after sealing, tombstones of names
    written during initialization compact like any other (the pins remember
    them for a load), so the globals compact at different points than before.
  - Error messages that name a callback by its global use the first binding
    in table order whose key is a UTF-8 string.

### Added

- `State::seal_globals` and `State::globals_sealed`. Sealing ends global
  initialization: from then on globals set to nil are reclaimed as in any
  table, so a script writing computed names through `_G` no longer grows the
  globals without bound. An unsealed State keeps every global's slot, as
  before. With the `snapshot` feature, sealing records the names written
  until then as pins, the save records the phase and the pins, and a load
  restores both; a sealed load's setup must replay the saving State's
  initialization (`docs/save-state.md`).
- `Program::disassemble`, returning a displayable `Disassembly` listing of the
  finalized bytecode, and the CLI flag `--dump-bytecode`, which prints that
  listing without executing the script. The listing format is diagnostic and
  may change between versions.
- `LoadError::InvalidTable` and its `TableLoadLocation` (`#[non_exhaustive]`),
  exported beside `LoadError`.
- Per-State GC policy: `State::with_gc_policy`,
  `State::with_callbacks_and_gc_policy`, `gc_set_policy` and
  `gc_set_auto_threshold`.
- `State::try_to_boolean`, `try_typ` and `try_raw_equal` report an invalid
  stack index; the existing forms keep reading it as `nil`.
- The snapshot byte limits are public in `limits`.
- Reference-pinned library limits are public in `limits`: `MAXCCALLS`,
  `LUA_MAXCAPTURES`, `MAX_FORMAT_LEN`, `MAX_FORMAT_FIELD_DIGITS`,
  `TONUMBER_BASE_MIN`, `TONUMBER_BASE_MAX` and `TABLE_SORT_MAX_LEN`. Values
  and error paths are unchanged.
- Typed library error context: `LibraryError` (the payload of
  `ErrorKind::LibraryError`), `LibraryFunction` and `StringPatternFunction`,
  read through `Error::library_function` and `Error::pattern_function`.
- `CostAnalysis::caveat()` and a `dellingr::cli` module with the analysis
  warning prefix. The `--analyze` warning now prints that caveat, with new
  wording.
- The CLI reads the script from stdin when the input is `-`, as reference Lua
  does, under the chunk name `stdin`. It used to open a file named `-`; that
  file is now run as `./-`.

### Changed

- The `hotpath-alloc` cargo feature now enables this crate's `hotpath`
  feature, so every instrumentation guard keyed on `hotpath` also applies to
  allocation-tracking builds.
- The `hotpath` profiler is now an optional dependency, enabled only by this
  crate's `hotpath` feature, so a default build no longer compiles the
  profiler or its runtime crates. Recursive parser functions are no longer
  measured under `hotpath`, so allocation tracking no longer exhausts the
  profiler's nested guards on deeply nested sources.
- The crate exports a `#[doc(hidden)]` `native_call!` macro, which wraps each
  internal call that can re-enter Lua so a debug-only test feature can trace
  it. Outside that feature it expands to the wrapped call unchanged; it is not
  part of the supported API.
- The test-only `fault-injection` cargo feature now fails to compile without
  debug assertions, so a dependency that enables it cannot reach a host's
  release build.
- The published package is an allowlist: the runtime sources, `docs/`,
  `reference/`, the README, license and lockfile, and the `hotpath` harness
  example. The integration tests, the Lua example corpus and the fixture
  regeneration tools are no longer shipped; they need the repository's
  scripts and gates and never ran from the package.
- **Replay-visible:** `math.sin`, `cos`, `tan`, `asin`, `acos`, `atan`,
  `atan2`, `exp`, `log` and the `^` operator no longer call the platform libm.
  They are in-crate ports of the CORE-MATH correctly rounded binary64
  functions (`src/crmath`, MIT, notices in `src/crmath/LICENSE-CORE-MATH`),
  with fused multiply-adds done by the hardware instruction or an in-crate
  software FMA that agrees with it bit for bit. Results are now the same on
  every supported host instead of depending on the host's libm, so they can
  differ in the last place from what the same script produced before on any
  given host, and from reference Lua. `math.log(x, b)` keeps the Lua 5.2
  formula (`log10` for base 10, otherwise `ln(x) / ln(b)`). Cost charges are
  unchanged and `^` is still not constant-folded. Supported hosts are those
  with IEEE binary64 arithmetic in the default floating-point environment;
  x87-only 32-bit x86 is not. `docs/determinism.md` has the details.
- **Replay-visible:** the metamethod event names (`__index`, `__newindex`,
  `__call`, `__len`, `__tostring`, `__metatable`) are interned once when a
  State is built and stay live for its lifetime, instead of being interned at
  each metatable lookup. Lookups no longer allocate, so collections can run at
  different points than before (GC timing is not a cross-version contract;
  see `docs/determinism.md`), and a `%p` id given to one of these names no
  longer changes when the string would previously have been collected and
  rebuilt. Snapshots carry such an id even when no saved value reaches the
  name; the snapshot format is unchanged. `State::string_count` counts the
  six names from construction on, and they count toward the adaptive GC
  threshold.
- **Replay-visible (snapshot contents only):** the compiler no longer emits
  upvalue-close instructions for block and loop scopes whose locals no closure
  captures, so the same source compiles to fewer instructions in functions
  that create closures, and snapshots of such states contain that shorter
  bytecode. Results and cost charges are unchanged, the snapshot format is
  unchanged, and older saved bytecode still loads and runs.
- Metatable lookups for `__index`, `__newindex` and string `__index` no
  longer reserve temporary stack slots, so near `MAX_STACK_SIZE` an access
  that used to fail with a stack overflow may now succeed.
- An assignment takes at most `limits::MAX_ASSIGNMENT_TARGETS` (255)
  targets; one more is the syntax error `TooManyAssignmentTargets`. A function
  that needs more registers than bytecode can name now fails with the syntax
  error `TooManyRegisters` (both carry the observed count and the limit), where
  it reported an internal compiler error, or with enough targets a verifier
  rejection that panicked debug builds. Assignments of 256 to 65534 targets
  that used to compile are now rejected.
- Bytecode verification names the actual defect for a frame size of
  `OPEN_COUNT` (`ValidationReason::FrameSizeUnencodable`) and for a
  `TFOR_CALL` result count above 255 (`TforCallResultCountExceeded`).
- `CostAnalysis::caveat()` and the `--analyze` warning now say that every
  native library charge, flat or data-dependent, is left out.
- The cost analysis labels a scope with no static cost `cost 0 (static)`
  instead of `cost 0 (free)`: a generic `for` can still call a costed
  iterator at runtime.
- The CLI reports a parse error the same way for a run, `--analyze` and
  `--dump-bytecode` (`Error: parsing source: ...`).
- The disassembly header names the non-parameter local count
  `non_param_locals` instead of `local_slots`.
- `TooManyRegisters` reports the line of the construct that ran out of
  registers, `local` statements and expressions included, instead of the
  function's last line.
- Reading a global through `_G`, or writing one that is already bound, no
  longer allocates a host string per access.
- `string.gsub`'s bad replacement capture names it: `invalid capture index
  %2` (was `invalid capture index`).
- Snapshot errors in an environment delta's hash slots name the new
  `TableLoadLocation::EnvironmentHashSlots`.
- docs/save-state.md states that values a `load_state` setup leaves on the
  value stack are discarded.
- Method-call inline caches no longer revalidate on every global rebind or
  restricted-environment swap. Results are unchanged.
- `ipairs` pushes its registered iterator directly instead of registering it
  again on every call.
- Pattern escape classes (`%a`, `%d`, ...) come from a constant table instead
  of a 256-byte scan per escape.

### Fixed

- A short string broken off by a newline reports `unfinished string` at the
  end of the line it broke off on (the newline's first byte, where the closing
  quote is missing), the same column the end-of-source case reports. It used
  to report the start of the next line after a bare CR or LF, and the column
  after the newline's first byte after CRLF or LFCR.
- A reachable `ipairs` iterator can be saved (registered as `ipairs.iter`).
- `table.move` charges and copies identically with and without a budget; a
  budget that runs out mid-move keeps the prefix it paid for.
- `table.move` accepts an exact singleton key above 2^53, like the other table
  functions.
- The CLI exits quietly with status 0 when stdout is a closed pipe instead of
  panicking with exit 101.
- **Replay-visible:** a string literal reachable only through a saved
  closure's code keeps its `%p` id across save and load, instead of rendering
  a fresh id after load. Snapshots of such states carry the literal's id; the
  snapshot format is unchanged.
- A table used as a FIFO (`q[tail] = v; q[head] = nil`) no longer grows with
  its history. The array part is now a logical length plus a stored window
  of it: an append drops the window's dead prefix once it is half the
  window, an all-dead window restarts at the next write, so storage, `pairs`
  and `next` starts, and snapshot size track the live span instead of every
  key ever pushed. Which keys are array keys, `#`, `pairs` order and where
  later writes land are unchanged, so script behaviour and cost are
  identical. A table that keeps one old key alive while the rest of a long
  array dies still stores the span between them. On a 32-bit host, growing
  an array past 2^31 - 2 keys now fails with `table growth failed` rather
  than exhausting memory.
- A field read, field write or method lookup could return or overwrite the
  wrong field of a table after very long table churn. Inline caches remember
  a table by its heap handle, and only the handle's 32-bit generation told a
  freed table from a new one in the same heap slot; after about 2^31 reuses
  of one slot, a site whose cache entry had sat untouched all that time could
  match the new table at the same shape version and use the old table's slot
  layout. Reaching it takes a long-running State that frees and reallocates
  tables on the order of billions of times, and replays reproduced the wrong
  result. Every collection now drops the cache entries naming what it freed,
  so no entry outlives its object. Results, `cost_used` and snapshots are
  unchanged; collections do a little more work, proportional to the inline
  caches of the live code.
- **Replay-visible:** saves are byte-stable when several saved functions
  carry string literals that `%p` rendered. Those literals got their string
  ids in the order the functions' bytecode happened to sit in memory, so a
  loaded State, a different allocator or another host could save the same
  logical state with different bytes. They now take ids in the save's own
  function order. Affected saves change bytes once; the snapshot format and
  `FORMAT_VERSION` are unchanged, and older saves still load.
- An explicit nil optional argument takes its default: `error(msg, nil)`
  raises `msg` at level 1 instead of an argument error, and
  `math.atan(y, nil)` and `math.log(x, nil)` act as the one-argument forms.
- `table.concat` accepts a number separator and converts it to a string, as
  reference Lua does.
- `string.gsub` checks its subject and pattern before the replacement, so
  `string.gsub()` and `string.gsub("x", {})` blame argument 1 or 2 instead of 3.
- **Replay-visible:** `x ^ y` with `|y| >= 2^1014` returns the overflowed or
  underflowed result directly. For rare inputs the old path carried an
  out-of-range intermediate into later phases, which could panic a debug
  build and left the release result to chance.
- Snapshot decoding caps a vector's initial reservation by its size in bytes,
  not by element count, so a forged length prefix can no longer reserve many
  times the input size.
- Host `get_table` keeps its key on the stack when a function-valued
  `__index` handler is refused before it runs (its frame does not fit, or a
  call-depth limit is reached), as it does for other refusals.
- An open `...` that is refused for lack of stack space leaves the
  function's live registers untouched instead of truncating them first.
- Table array growth that cannot be reserved (a logical length past the
  array limit, as an edited save can demand, or an allocator refusal) raises
  the new `ErrorKind::TableGrowthFailed` ("table growth failed"), which kills
  the callback and leaves the table unchanged, instead of panicking or
  aborting. A load that hits it reports `TableRestoreFailure::GrowthFailed`.
- **Replay-visible:** a `string.gmatch` iterator whose result push is refused
  no longer skips the match or tears its saved position; the next call
  returns that match.
- A table array whose live slots were cleared in any order, not only from the
  front, now releases its stored window at the next append or array write
  outside it. Storage only: results, `pairs` order and cost are unchanged,
  and a write whose growth reservation used to fail may now succeed.
- `table.sort` whose sequence copy cannot be allocated (an edited save's
  impossible gap) raises `table growth failed` instead of aborting.
- **Replay-visible (`%p` ids):** identities a `load_state` setup rendered are
  merged with the saved ones instead of being dropped. A value the save
  records takes its saved identity; any other value setup rendered that
  survives the load, such as an anchored table, keeps its setup id, or gets a
  fresh id above the saved counter when a saved value holds that id. A setup
  that replays the saving State's initialization now continues with the same
  ids as the saving State; before, such a value took a new id when next
  rendered (`docs/save-state.md`).
- `HostCallbacks::on_error` fires for every execution error before host code
  can see it, at any depth. A Rust callback that called `State::call` (or
  `call_anchor`, `get_table`, `tostring_bytes`) and handled the error instead
  of returning it used to leave `on_error` silent unless the failure came out
  of a Lua frame: calling a non-callable value, a `__call` chain hitting its
  depth or argument limit, a Rust function returning `Err`, result padding
  overflowing the stack and the native call depth limit were never reported.
  They now notify once, with the same location, traceback and source name as
  the error the callback receives; an error that propagates instead is
  reported exactly as before. Host `get_table` and `tostring_bytes` now also
  notify for failures that never reached a call: a receiver that cannot be
  indexed, a non-function `__index` handler, an `__index` chain at the
  metamethod depth limit, a `__tostring` result that is not a string or
  number, and running out of `%p` identities. Operand misuse (invalid
  index, missing key) still does not notify. The `on_error` doc states the
  rule.
- Loading a save into a build that folds two rendered Rust functions the
  saving build kept apart no longer fails as a duplicate: the folded function
  keeps the smaller of the two identities. Two such functions used as keys of
  one table still reject the load, now documented.
- **Replay-visible:** whether an `__index` lookup succeeds at the
  metamethod depth limit or the stack cap no longer depends on the lookup
  site's history or on how the lookup is written. A field-read site that
  had once resolved an inherited method, a function `__index` or a miss
  sent every later receiver down the general path for good, so the same
  `obj.k` at the depth limit or the stack cap succeeded on a fresh State,
  after a snapshot load or a collection, and failed on a State that had run
  longer; `obj["k"]` and string receivers counted the one-level step
  differently again. The rule is now one for every route: an `__index`
  table that answers from its own contents takes no depth unit, a table
  that continues to its own `__index` takes one, and a function or string
  handler takes one (`docs/limits.md`). Compared with before, one-level
  `__index` reads at the limit now succeed on every route; a string
  receiver whose library table continues to another `__index` takes one
  more unit; a handler that is neither a table nor callable reports its
  type error without taking a unit; and an `__index` chain of tables needs no
  value-stack slot per table and no native stack per step. Inherited method
  calls through one site that serves several classes no longer slow the
  others down. Results and `cost_used` are otherwise unchanged.
- **Replay-visible at the metamethod depth limit:** `__newindex` now counts
  depth by the same rule as `__index`. A handler table that takes the store
  into its own contents (it holds the key, or has no `__newindex` of its
  own) takes no depth unit; a table that continues to its own `__newindex`
  and a function handler each take one. Every handler dispatch used to take
  one, so at `MAX_METAMETHOD_DEPTH` `obj.k` through a one-level `__index`
  table succeeded while `obj.k = v` through a one-level `__newindex` table
  raised `MetamethodDepthExceeded`. Compared with before, a store through a
  chain of tables needs one unit fewer, on `t.k = v`, `t["k"] = v` and
  table-library writes alike; a `__newindex` handler that is neither a table
  nor callable reports its type error at the limit instead of the depth
  error; and an error from the terminal table's raw insert (a NaN key, for
  instance) is no longer masked by the depth error there. Results and
  `cost_used` are otherwise unchanged.

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
