# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Changed

- `analyze_cost` reports lower `ScopeCost::instructions` counts for unchanged
  source. Bytecode finalization now strips retired call marks and no-op
  `CloseUpvalues`, which were emitted but never executed. Charged cost is
  unaffected - `own_cost`, `total_cost`, and runtime `cost_used` are identical,
  since the stripped instructions were free under the cost model. Only
  embedders comparing raw instruction counts against stored baselines will
  observe a difference.

### Fixed

- Saved bytecode is now verified for operand-stack discipline before it is
  materialized. Previously a forged save whose chunk began with `OP_POP` or
  `OP_SWAP` passed every structural check and panicked the host process on
  load, since `pop_val` and `OP_SWAP` index the stack unguarded. Verification
  abstractly interprets each chunk - operand height relative to the frame base,
  the vararg-call and table-constructor marker stacks, and exact state
  agreement at every control-flow join - and rejects violations as
  `LoadError::InvalidBytecode`. This completes the documented promise that
  malformed save structure cannot trigger an indexing or stack-underflow panic
  during load.

## [0.4.0] - 2026-07-25

### Changed (breaking)

- The cost model now charges data-dependent native work in the string, pattern,
  and `table.concat` libraries. This work was previously entirely uncharged.
  There is no conversion formula: the increase depends on string lengths,
  output expansion, match success, captures, and backtracking shape. Every
  embedder must re-measure its tick budgets.
- Snapshots now use format version 6 and persist `COST_MODEL_VERSION` alongside
  the format version. Old snapshots are rejected rather than being loaded with
  counters from a different cost model.
- `COST_MODEL_VERSION` is exported so hosts can identify the runtime accounting
  model independently of the crate version.

### Fixed

- `string.len` and `string.sub` no longer clone their whole input. `gmatch` no
  longer recopies its subject or recompiles its pattern on every iteration, so
  a full scan is linear in its subject rather than quadratic. Measured on
  `examples/strings/patterns.lua`: 160ms to 65ms, a 2.4x improvement that more
  than absorbs the new per-primitive charging.
- Compiled `gmatch` patterns are cached with a bound on both entry count and
  total cached bytes, and the cache is cleared on collection. Without the bound
  a script using many distinct patterns grew host memory without limit.

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
  load (`FORMAT_VERSION` is 6). Dynamic-call and table-constructor base stacks
  unwind on a frame error, keeping the State quiescent so a host that catches
  an error can still snapshot or reuse it.
- The lexer accepts Lua 5.2 hex-float literals (`0x1.8p+0`, `0x.8`, `0x1p-2`),
  so numeric `string.format("%q")` output is re-parseable by dellingr itself
  (closing C30). Hex literals now convert through the shared numeral parser,
  which also rounds oversized hex integers to the nearest f64 like reference
  Lua instead of failing past `u128` range; malformed forms (`0x.`, `0x1p`,
  `0x1p+`) are a syntax error.

- Clearing the field a `pairs` traversal is currently sitting on is now
  supported - the filter-in-place idiom reference Lua permits. `Table::remove`
  leaves the key in place and stores `Val::Nil`, tracked by a `dead_count`;
  every index-based accessor hides dead slots (`next` excepted, since finding a
  dead control key is the point), so a warm `SET_FIELD` cache cannot write
  through a tombstone and resurrect a deleted field. Reinserting a key appends
  rather than reviving in place, preserving iteration order, and compaction runs
  only when tombstones are dense, keeping a `t[k] = v; t[k] = nil` cycle from
  going quadratic. Costs ~2.5% on `iter/pairs`.
- Source limits are bounded and documented (new README "Source limits"
  section): parser recursion is capped at 200 levels (`TooManySyntaxLevels`)
  and upvalues per function at 255 (`TooManyUpvalues`). Previously deep nesting
  overflowed the native stack - an abort, not a returnable `SyntaxError` - and
  the 256th upvalue silently got index 0 and read the wrong variable. One
  accepted divergence: a paren costs two depth ticks, so parens nest ~99 deep
  against reference Lua's ~200; every other construct gets the full 200.
- Saves are verified before they are materialized. Save files are user-editable
  and the bytecode inside them was previously trusted completely, so a
  hand-edited save could drive the interpreter out of bounds and abort the host.
  A verifier behind a `VerifiedSavePayload` (no unchecked constructor) checks
  chunk and graph invariants, every operand used as an index, jump targets,
  closure capture arity, and acyclicity plus a 200-deep bound; rejections raise
  the new `LoadError::InvalidBytecode`. The same checks run on compiler output
  as a debug assertion over a shared bytecode view, so the test corpus
  continuously proves the compiler emits only what the loader accepts. This is
  phase 1: operand-stack dataflow analysis is deliberately deferred (tracked as
  finding #59), so the no-abort-on-load guarantee is not yet established.
- Snapshots persist mutations to the standard-library tables. The encoder used
  to emit a bare token for any environment object and never walk its entries, so
  `math.myconst = 42`, `string.trim = f`, or a whole subtable reachable only
  through `table.foo` vanished on load with no `SaveError` and no diagnostic.
  `Table::version()` could not drive the detection - it deliberately does not
  bump on a value update, so `math.floor = myfloor` evades it - so the
  environment baseline (ordered entries plus metatable) is captured at
  construction and the save emits a delta of deletions, upserts, an optional key
  order, and a tri-state metatable change. Deletions are required, or a deleted
  stock entry is resurrected from the freshly built library. Delta values go
  through the normal iterative walker, so a graph reachable only through `math`
  (including a cycle back into `math`) lands in the ordinary arenas. An
  unmodified environment costs four bytes and an `O(E)` scan. Replay preserves
  keys the save never saw, which is what lets a newer build or a load-setup
  closure add `math.future` without failing the load.
- Object identities survive a load: `format_pointer_ids` and its counter are
  persisted, so a `tostring` result stored in a saved global can no longer come
  back byte-identical to a *different* object's identity. Ids are restored
  exactly and the counter is saved even when the values it counted are gone, so
  a collected object's id is never reused. Only identities reachable in the
  payload are written, since the map is deliberately not a GC root.
- `error(msg, level)` is implemented: `0` suppresses the location prefix, `N`
  blames the caller `N-1` frames up, and an out-of-range level renders no
  prefix.
- Lua strings are capped at 16 MiB (`MAX_STRING_BYTES`, inclusive), in the same
  class as `MAX_CALL_DEPTH` and the one-million value stack, with a dedicated
  `ErrorKind::StringSizeExceeded { size, limit }` so hosts need not parse text
  to recognise a resource-limit termination. See "Fixed" for the hole this
  closes. No `consume_cost` call is added anywhere and every under-cap script's
  `Cost used` is byte-identical; per-byte charging remains finding #16.
- `ErrorKind::ScriptError`, so a script-raised `error()` is no longer reported
  as `InternalError` - which is documented as "corrupt bytecode or VM bug", and
  so had embedders filtering for crash reporting collecting ordinary script
  errors. `RuntimeError`'s documentation was wrong in the other direction (the
  VM itself raises it) and now says so.

### Changed

- **Breaking (save format):** the snapshot format is bumped v2 to v5; existing
  save files are rejected with `UnsupportedVersion` by a strict equality gate,
  with no fallback parsing. v3 persists whether a cost budget is configured,
  without which a restored state took the unbudgeted `table.move` path and
  bypassed its own remaining budget; v4 adds the environment delta and the
  object-identity registry; v5 carries the widened literal pools. The golden
  fixture is regenerated at each bump.
- **Breaking (API):** three signature changes, all so that host misuse becomes
  an error before any mutation rather than a panic or an unbounded allocation.
  `State::to_string` takes `&mut self`, because minting a deterministic identity
  requires mutable access - accepted deliberately over preserving the address
  leak or adding interior mutability purely for display. `State::set_top` and
  `State::pop` return `Result<()>`. `push_bytes` and `push_string` return
  `Result<()>`. Roughly 89 production call sites propagate with `?`; nothing
  depended on the old behaviour.
- **Breaking (API):** `MAX_STACK_SIZE` is now a real, global cap on the shared
  Lua/Rust value stack rather than a limit applied only when preparing Lua
  frames. Previously a host `RustFunc` pushing in a loop never met it. Every
  path that can grow the stack is now checked, batch-preflighted, or provably
  net-neutral, so the cap holds for bytecode operand pushes, frame setup and
  metamethod dispatch as well as the host API. `push_nil`, `push_number`,
  `push_boolean`, `push_rust_fn`, `new_table`, `get_global` and
  `State::open_libs` therefore return `Result<()>`; exceeding the cap is a
  catchable `StackOverflow` and a rejected operation leaves the stack, the
  string pool and the function registry unchanged. Enforcement charges nothing
  against the instruction-cost budget, and no existing workload approaches a
  million values. This bounds the value stack, not total host memory.
- The Lua pattern matcher is rewritten as a one-pass compiler plus a safe
  index-based matcher, replacing a raw-pointer transliteration of the reference
  C. Every atom - literal, `.`, `%a`-style class and bracket class alike - is an
  interned 256-bit byte class, so matching an item is one bitset test with no
  branch on atom kind, identical classes are deduplicated through a `BTreeMap`
  to keep the compiled form deterministic, and compiling once removes
  `classend`'s rescan of class bytes at every subject position the scan loop
  tries. Two conformance bugs that could not be patched in place fall out of the
  new shape; see "Fixed". Error timing is unchanged and remains contract:
  compile errors surface from `from_bytes_try`, match-time errors from
  `matches_bytes_from`, and `gmatch` still compiles lazily on first iteration.
  No cost is charged anywhere in the matcher; the central step site is left
  ready for finding #16.
- `table.sort` is an iterative heap sort with a fallible comparator, replacing a
  bubble sort with no early exit that always performed exactly `N(N-1)/2`
  comparator calls - about 5*10^7 full call round trips on 10k elements, charged
  10^4. Heap sort is the choice specifically because a Lua comparator is
  script-controlled and may be inconsistent: every index derives from fixed heap
  bounds, so a comparator that always returns true yields a wrong-but-safe
  permutation rather than an out-of-bounds access (`sort_by` could not be used -
  it is infallible and can panic on a non-total order). Inconsistent comparators
  stay deterministic-but-undiagnosed; reference's "invalid order function" is an
  artifact of its quicksort bounds check, and reproducing it would mean adding
  comparator calls purely to detect it. Rooting is taken only on the comparator
  path, so the default comparison no longer pays for two extra copies of the
  array, and on comparator error the detached array is dropped before writeback,
  leaving every original slot intact.
- Per-function capacity ceilings are lifted. Literal indices were 8-bit
  operands, so `{1, ...}` x300, 300 named constructor fields, 300 `t.fN = N`
  assignments and 300 distinct number literals each failed to compile where
  reference Lua accepts them. The literal pools move to 16-bit ids carried in
  `Bx` (u16 rather than u32 deliberately: 65536 is 200x the motivating cases,
  and u32 would require multiword encoding and a different literal-loading
  design), array constructors flush in batches with `SET_LIST` carrying a batch
  ordinal in its previously reserved `Bx`, and table templates become
  `Vec<Vec<u16>>` so no hidden ceiling survives behind the widened instructions.
  `GET_GLOBAL`, `GET_FIELD` and `SET_FIELD` had no free operand bytes, so their
  inline-cache slots narrow to 8 bits with 255 meaning "uncached": cached sites
  cap at 255 per function and the rest take the slow path, measured at 1.81x on
  a map-backed table (about 18ns per write) with instruction cost unchanged
  either way. Nothing in the repo comes close - the most field-heavy example has
  83 field instructions in the entire file. `SET_FIELD` splits into `SET_FIELD`
  (cached, implied receiver offset zero) and `SET_FIELD_AT` (uncached, explicit
  offset), so the hot path keeps its cache and the rare multi-lvalue path pays
  instead of everything paying. Verification treats the sentinel accordingly and
  bounds-checks every `Bx` literal and every u16 template key, with a
  forged-snapshot test per opcode.
- The `string` library accepts numbers wherever it accepts strings, through one
  shared helper. Both references use `luaL_checklstring` throughout, which takes
  numbers and rejects booleans, tables and functions. The previous exact-type
  check was accidental rather than deliberate: concat already coerces numbers
  and `string.format` already accepts a numeric format string, so the strictness
  was not applied consistently even within the crate. Arithmetic is the opposite
  case and stays strict - `"10" + 1` remains an error, because that strictness
  *is* applied consistently across `as_num`, unary negation and numeric `for`,
  breadth that reads as VM policy rather than an oversight. It is now documented
  under "Won't implement" instead of being an undocumented divergence.
- `tostring` on a heap object renders `table: 0x1` / `function: 0x2` instead of
  `object: ObjectKey(7v1)` - the slotmap key's Rust `Debug` form, with the type
  word wrong for every object because `Display for ObjectPtr` had no heap
  access. The type now comes from the heap-aware `Val::typ` and the digits from
  the same deterministic `format_pointer_id` registry `%p` uses, so `tostring(t)`
  and `string.format("%p", t)` agree as they do in reference. `Display for
  ObjectPtr` and `Display for Val` are deleted rather than left as traps:
  rendering an object correctly needs both the heap and the `State`, so any
  heap-free impl can only reintroduce the leak.
- `math.log` special-cases base 10 with `log10`, so `math.log(1000, 10)` is
  exactly `3` rather than `2.9999999999999996`. Base 2 is deliberately left
  alone: it is a genuine 5.2/5.4 split and dellingr currently matches 5.2.
- `rust-version` returns to the 1.92 already documented in README and AGENTS.md.
  The real floor is 1.88 (let-chains and `slice::as_chunks`, with let-chains
  requiring edition 2024); nothing in the locked dependency tree needs the 1.97
  the manifest claimed, which arrived alongside an unrelated bump. The README
  dependency snippet moves from 0.2 to 0.3 to match the crate.
- `drain_mark_worklist` debug-asserts that every entry it pops was already
  coloured `Reachable`. `GcHeap::mark` sets an object's colour *and* queues it;
  pushing a pointer straight onto the worklist instead traces that object's
  children while leaving the object itself `Unmarked`, so sweep frees it and
  later reads dangle. This happened for real in this series (see the save/load
  rooting fix below) and the full suite passed with the bug present. Release
  builds are unaffected.
- Zero-step numeric `for` is documented as a deliberate divergence: dellingr
  skips in both directions, where 5.2 skips ascending but loops forever
  descending and 5.4 errors.
- `table.move` charges per element instead of a flat 1. The range came straight
  from script arguments and was not bounded by table size, and neither reads of
  an empty table nor writes of nil allocate, so `table.move({}, 1, 2^30, 1)` was
  ~10^9 table operations for cost 1. Charging stops before the first element
  whose charge finds the budget exhausted, which leaves a partially moved table;
  both overlap directions charge in real copy order, so that partial state is
  deterministic. With no budget configured there is a single `count.max(1)`
  charge and no per-element branch. The neutral `CostMeter` infrastructure for
  charging string and pattern byte-work lands here but is not yet wired up -
  that is a cost-model version bump, not a bug fix.
- `tostring` on a Rust function renders a constant `<function>` instead of the
  real function pointer, which ASLR changes between runs of the same binary -
  a replay-visible determinism break, since scripts can branch on the string.
  Lua promises uniqueness for `%p` and not for `tostring`, so nothing that was
  actually guaranteed is lost; this also settles an inconsistency where
  `Debug`/`Display` printed the payload slot address and `to_string_with_heap`
  the code address.
- Leveled long comments (`--[=[ ... ]=]`) are now lexed. Long *strings* remain
  rejected as `LongStringUnsupported`: comments are lexical trivia and need no
  value type, allocation, opcode, or cost-model change.
- `break` is no longer terminal: `while true do break; end` compiles, as does
  dead code after a `break`, matching reference Lua.
- Numerals glued to identifiers are rejected (`print(3or 4)` is now "malformed
  number near '3o'"). This tracks Lua 5.4; 5.2 accepts the old form.
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

- Unbounded string allocation. `OP_CONCAT` charges nothing while
  `concat_helper` does `O(total bytes)` of work and allocation, and the
  surrounding loop is deliberately free, so building a 1 MiB string reported
  `Cost used: 0` and completed even under `--limit 100` - another twenty
  iterations is a terabyte attempt, and a host could not defend by lowering the
  budget, because the cost is zero at any budget. Fixed with the 16 MiB resource
  limit above, enforced in two layers: admission into the interner is now
  fallible as the backstop, and every expanding producer checks before growing
  its buffer (concat, `table.concat`, `string.format` including `%q`'s expanded
  temporary, all three `gsub` branches, `gmatch`'s leading-`^` rewrite, `print`'s
  assembled message, host `push_bytes`/`push_string`, and compiled string
  literals). Checking only at intern time is too late - the dangerous `Vec`
  already exists by then. Concat renders numeric operands during the length pass
  rather than assuming a fixed 32-byte bound, which was wrong in both directions
  (over-counting `1` as 32 bytes and so rejecting legal results just under the
  cap, which would itself have changed what a script costs; under-counting
  `1e308`, which Rust renders as 309 bytes), accumulates saturating so an
  overflowing total is rejected rather than wrapping to a small value that would
  pass, and interns before truncating the operand stack so a rejected concat
  leaves its operands in place. Literals are capped by decoded size, not source
  spelling. `print` preflights string arguments against their raw length, since
  lossy UTF-8 expands an invalid byte to three and a legal 16 MiB string of
  `0xff` would otherwise materialize as ~48 MiB. Snapshot loading enforces the
  identical inclusive boundary on payload strings and bytecode literals.
  Recorded rather than glossed: a per-string cap is not a total-memory quota.
- A method or function call whose callee expression was not a plain name, and
  which had at least one argument that was itself a call, dispatched the
  receiver instead of the method - `("ab"):find(id("b"))` raised "attempt to
  call a string value", as did `({ f = f }):f(id("q"))`, `("a" ..
  "b"):find(id("b"))` and `make("a")(id("b"))`. `OP_MARK_CALL_BASE` was emitted
  *before* the callee expression was evaluated, so it had to guess how many
  values the callee had already left on the stack; the guess covered exactly two
  shapes and was wrong for everything else. The marker now comes after the
  callee is evaluated, where the answer is always the same - the callee is the
  single value on top, so the base is one slot below it - which holds whether
  the callee pushed itself, was already on the stack as a parenthesized
  expression or constructor, replaced its receiver in a field access, collapsed
  a receiver and key in an index, or is the single result of an inner call. The
  adjustment operand is consequently always 1 and the verifier now requires
  exactly that. Extending the table of adjustments instead cannot work: an inner
  call's `OP_CALL` is emitted by the evaluation step that *follows* the marker,
  so at marker time its width is not yet fixed. Two further defects predating it
  are covered by the same reordering: an index receiver pushes a table and a key
  that `OP_GET_TABLE` collapses into one, and the marker's base was computed by
  unchecked subtraction from the absolute stack length, which would accept a
  base below the active frame whenever the stack was deep enough and then take a
  caller-owned slot as this call's callee.
- Two pattern-matcher conformance bugs, both consequences of the old module's
  shape and so fixed by the rewrite above. `matchdepth` was decremented on entry
  to `patt_match` but restored only on the two fallthrough exits, so the six
  tail-call continuations leaked a level each, and the scan loop never reset it
  between anchor positions - a 250-byte subject died on its first `find`. Depth
  is now passed by value, every C `goto init` transition is a loop iteration,
  and only the five genuinely recursive edges consume a level, reproducing
  reference's boundary exactly (`("a?"):rep(199)` matches, `rep(200)` raises
  "pattern too complex", 201 sequential literal items match fine). Separately,
  `%f` treated slice-start as string-start while every caller passed a suffix,
  so each resumption point became a false left boundary and
  `("abcd"):gsub("%f[%w]%w", "X")` produced `"XXXX"` instead of `"Xbcd"`; the
  matcher now takes the whole subject with an init offset and reports absolute
  positions, deleting the base parameter chain threaded through `string.rs`'s
  capture helpers along with five re-slice sites. Three further defects caught
  by review during the rewrite are fixed and pinned: `*` and `-` items that
  cannot match once take the depth-free accept-empty transition rather than
  recursing, position captures consume a depth level as reference's
  `start_capture` does, and bracket classes preserve reference's mutually
  exclusive escape/range/literal branch order, without which `[z-a]` wrongly
  contained `z` and `[%--a]` expanded to a broad range.
- `table.insert` at a position shifted elements with `shift_remove` followed by
  `insert`, from high index to low. `IndexMap` appends a re-inserted key, so the
  shifted keys ended up in reverse insertion order followed by the new key, and
  each `shift_remove` was `O(tail)` - making a single `table.insert(t, 1, v)` on
  a 50k array roughly 10^9 memmoves charged as 1. It now carries a value forward
  across the existing keys, writing each slot in place, so the operation is
  `O(N)` and insertion order is preserved (`pairs` order is unspecified by Lua,
  so that half is a quality fix rather than a conformance one). Rotation works
  directly on inline, map and tombstoned storage, so the `ensure_map` call is
  gone. The same function unconditionally cached `len + 1` as the array length
  even when that was not a border - for `{1,2,3}` with `t[5]` set, the
  post-shift `t[5]` is non-nil so 4 is not a border, and the cache reader trusts
  any cached value without validating it. It now caches only when the inserted
  value is non-nil and `len + 2` is nil.
- `table.sort`'s default comparator ordered numbers before strings and returned
  `Equal` for everything else, so `table.sort{1,"a"}` and `table.sort{{},{}}`
  silently succeeded where reference raises a type error. It now errors on
  incomparable operands from inside the comparator rather than a pre-scan,
  because reference accepts `table.sort{true}` - a singleton is never compared.
  `TypeError::Comparison` renders equal types as "attempt to compare two table
  values" instead of "table with table".
- Errors carried no position at all, rendering `0:0: internal error: oops` where
  reference gives `input:1: oops`, even though the traceback printed directly
  underneath had the right line. Position is now filled from the frame the error
  surfaced in, through one helper used by both error-return paths out of a Lua
  frame (the result-count overflow path previously bypassed it). `Display` takes
  the source and the line from the same stack frame, because reading the source
  from the traceback while taking the line from the error itself can splice a
  host `RustFunc`'s own line onto an unrelated Lua chunk's name.
- `Val::typ_simple` mapped every object to `Table` "for display purposes", so
  indexing a function reported "attempt to index a table value". All 36 call
  sites had heap access; they now use `Val::typ` and `typ_simple` is deleted.
- A string `__index` handler raised a type error instead of chaining. One shared
  helper now resolves a key against the global string table for direct field
  access, generic bracket indexing and string `__index` alike. This also fixes
  `("abc")[1]`, which errored but is nil in reference - the bracket path
  rejected strings as non-tables while the field path already fell back.
  `getmetatable("abc")` is still nil and string `__newindex` stays invalid; the
  no-string-metatable design is unchanged.
- `to_string_with_meta` accepted any `__tostring` result while its sibling
  `bytes_with_tostring_meta` correctly rejected non-strings, and `print` and
  `tostring` used the permissive one. They are now one path.
- A use-after-free in save/load rooting: `mark_gc_roots` pushed canonical
  environment pointers straight onto the mark worklist, which traces an object's
  children without colouring the object itself, so sweep still freed it. With a
  library shadowed and nothing else referencing the canonical table, the next
  save dereferenced a dangling pointer. It now goes through `GcHeap::mark`, and
  the debug assertion described above makes the class of mistake mechanical.
- `load_state` ran the host's setup closure and then `materialize_payload`,
  which ended by clearing the anchor registry - so an anchor created during
  setup was stale the moment `load_state` returned, with no error. The clear now
  happens before setup. Setup anchors must survive: the registry is a GC root,
  so otherwise the final collection frees values only setup holds.
- `instr_tfor_call_rust_fn` computed the actual result count as `get_top() as
  u8`, so a host iterator `RustFunc` leaving more than 255 values on its frame
  wrapped 256 to 0; the `Greater` arm then pushed spurious nils and the
  rotate/truncate bookkeeping left hundreds of stray values inside the loop
  frame - silent stack corruption. It now compares in `usize` and retains the
  topmost reported values, matching the sibling path in `State::call`.
  Deliberately not a "too many results" rejection: `get_top()` covers the whole
  frame including arguments the callback never removed.
- `State::set_top` grew the stack by pushing nils in a bare loop with no
  `check_stack_space`, so `set_top(isize::MAX)` would OOM the process, bypassing
  the 1M-value cap; `State::pop` panicked on counts above the top and silently
  accepted negative counts, because its assertion only checked the upper bound
  and `0..n` is empty for a negative `n`. Both now validate fully before
  mutating, so a rejected call leaves the stack untouched - which also settles
  the inconsistency where they panicked while `insert`, `remove`, `replace` and
  `push_value` returned `InvalidStackIndex`. `set_table_raw` and
  `set_metatable_of` popped their operands before validating, so a host calling
  them with too few visible values reached `pop_val` and panicked; they now
  check that the operands sit above the table index, which additionally prevents
  popping the table itself and then indexing off the end of the stack.
- Only `OP_CALL` refreshed `CallInfo.ip`, so calls made by `OP_TFOR_CALL` and by
  `__index`/`__newindex`/`__len` dispatch left the caller's ip pointing at the
  previous `OP_CALL`, and tracebacks reported the wrong caller line for errors
  raised inside iterators and metamethods. Deriving lines at trace-build time is
  not possible - `build_stack_trace` holds only the innermost live `Frame` and
  derives outer entries from `CallInfo` alone - so the ip is now refreshed at
  each dispatch site that can re-enter Lua. Only outer traceback entries were
  affected.
- A call spanning several lines was attributed to its closing line, because
  `OP_CALL` is emitted after the argument list is parsed. The opening line is
  now carried on `PrefixExp::FunctionCall` and used at emission, so tracebacks
  blame the same line reference does. The line and argument count are packed
  into four bytes to keep that variant from dominating an enum the parser copies
  constantly.
- `add_local` grew `num_locals` whenever `locals.len()` exceeded it, but
  parameters were pushed without updating it, so sibling scopes in a function
  with parameters re-triggered growth and every call pushed extra nils. It now
  maximizes `locals.len() - num_params`.
- `_G` no longer stringifies keys, so `_G[1]` and `_G["1"]` are distinct as they
  are in reference. String keys still route through `State::globals`; non-string
  keys are stored raw in the proxy.
- Oversized `GET_FIELD` sites raised `InternalError`, which since the
  error-taxonomy change means "VM bug"; it is now a line-bearing `SyntaxError`,
  consistent with the `SET_FIELD` path beside it. `Frame::jump` rejected `ip ==
  code.len()` only after the fact; the bound is now strict. Both are defence in
  depth rather than reachable panics - the bytecode verifier already requires
  jump targets strictly below `code_len`.
- Optional standard-library arguments treat an explicit `nil` as absent, like
  both references. The prevailing `if num_args >= k { check_type(k, ...) }`
  pattern errored on a `nil` reference would simply default, so the ordinary
  plain-find idiom `("a.b"):find(".", nil, true)` raised a bad-argument error
  instead of returning `2, 2`. A shared `check_optional_type` helper replaces
  the per-call-site guards (which is what produced the drift: `table.remove` and
  `table.move` handled `nil` correctly by hand while nothing else did) and
  delegates to `check_type`, so wrong non-nil arguments keep the same error kind
  and payload. Separately, `table.concat` no longer ignores an explicit range on
  a sparse table (`table.concat(t, "", 2, 2)`), and `unpack` no longer truncates
  negative start indices through `as usize` (`unpack(t, -2, 2)` returns five
  values from `t[-2]`, not three from `t[0]`).
- Four semantic divergences from Lua 5.4. `for` control expressions are
  evaluated in the enclosing scope instead of with the loop variable already in
  scope, so `local i = 5; for i = i, 7 do` prints `5 6 7` rather than raising
  (or, with slot reuse, silently reading a stale value for the bounds); slot
  layout is unchanged, so any program whose control expressions do not mention a
  loop-variable name compiles byte-for-byte as before. NaN ordered comparisons
  return false - `partial_cmp`'s `None` mapped to `Ordering::Equal`, and `<=`
  and `>=` are negated `>` and `<`, so both returned true. `1 % math.huge` is
  `1.0` instead of NaN, and `math.modf(inf)` returns a zero fractional part
  instead of NaN.
- Seven pattern-matcher defects, two of which aborted the host from a script:
  32 position captures indexed past the results array (the validator did not
  count position captures at all), and `"(a)%0"` overflowed formatting its own
  error message. The capture ceiling was also off by one the other way, so only
  31 were permitted where reference allows 32. Escaped uppercase classes matched
  the wrong character entirely - `match_class` lowercased before its literal
  fallback, so `%E` matched `"e"` and failed on `"E"`, affecting every escaped
  uppercase letter that naive quoting helpers emit. `%s` now matches vertical
  tab (C's `isspace` includes 0x0B, Rust's `is_ascii_whitespace` does not), and
  patterns ending in `%%` are no longer rejected outright, so
  `("50%"):gsub("%%", " percent")` works.
- Six lexer conformance defects. `skip_comment` recognised only `--[[`, so a
  leveled opener like `--[=[` fell through to the single-line branch and the
  comment *body* was lexed as live source - a confusing syntax error usually,
  but silent execution when the body happens to be valid Lua. Short comments
  had the same class of bug under CR line endings, swallowing the following
  statement. Bare CR was mishandled throughout: accepted inside string literals,
  `\<CR>` errored instead of mapping to one newline, `\<LF><CR>` produced two
  bytes, and a CR-terminated file never set `starts_line`, so the ambiguous-call
  rejection did not fire. Vertical tab is now whitespace between tokens and
  after `\z`, sharing `numeral.rs`'s `is_lua_whitespace` with the pattern
  matcher rather than growing a third definition.
- The GC marks the heap iteratively, so deep data cannot abort the host.
  `GcHeap::mark` recursed roughly five native frames per level of nesting with
  no bound - `local t = {} for i = 1, 500000 do t = { t } end` cost about 2 per
  iteration, sat inside any normal budget, and died with a stack overflow and a
  core dump. Marking now uses an explicit worklist, colouring before push so
  cycles and shared subgraphs enqueue each object at most once; the scratch
  `Vec` is owned by `GcHeap` and taken with `mem::take`, so it costs no
  allocation across the ~10,000 collections a GC-heavy script performs. Costs
  ~2.6% on `alloc/closure`.
- `save_state()` walks the object graph with an explicit task stack instead of
  recursing, so a deep graph no longer overflows the native stack inside an API
  whose signature promises `Result<_, SaveError>`. Output is byte-identical:
  object ids are assigned at first encounter, so the layout encodes the exact
  depth-first preorder, and children are pushed in reverse to preserve it. A
  golden fixture committed beforehand pins that order - neither existing
  byte-stability test could detect a reordering, since both run whatever
  algorithm is present twice. Diagnostic paths come from an append-only
  breadcrumb arena keyed by a `PathId`, reconstructed only when an
  `UnregisteredFunction` error is actually raised.
- Two save-corruption bugs in the writer, exposed by the new load verifier. Both
  the bytecode and upvalue arenas assigned an id from the current length but
  pushed the entry only after recursively encoding children, so a first-seen
  parent and its first-seen child received the same id - a closure over the
  parent serialized as chunk 0 and decoded to the child, silently restoring the
  wrong program. Existing round-trip tests missed it because they only invoked
  closures that already existed.
- Four GC root holes that let a script abort the host process. `mark_gc_roots`
  is documented as the single source of truth for reachability, but frame
  varargs (drained into a Rust-owned `Vec`), `#t` with a `__len` metatable
  (receiver popped before interning `"__len"`), `table.sort`'s array copy (held
  across an arbitrary comparator), and `with_restricted_env`'s `mem::replace`d
  globals each held a live `Val` across an operation that can collect, producing
  an "object was freed" panic. `active_call_roots` becomes a `TransientRoots`
  registry covering both scoped values and suspended environments, with
  watermarked helpers giving LIFO nesting, and `validate_quiescent` requires it
  to be empty so a leaked root is visible rather than silent.
- `Val::RustFn` has real value identity. `PartialEq` and `Hash` coerced match
  bindings to `*const RustFunc`, yielding the address of each `Val`'s payload
  slot rather than the function pointer inside it, so `print == print` and
  `rawequal(print, print)` were false and host functions were unusable as table
  keys (repeated assignment appended duplicate entries). `%p` on the same
  function minted a fresh id every call, defeating the deterministic identity it
  exists for, and the method inline cache could never validate a cached
  `__index = <RustFn>` handler. Now compares with `std::ptr::fn_addr_eq` and
  hashes the address value.
- `line_info` stays aligned with `code`. `Parser::push` appends to both in
  lockstep and is the only thing keeping them aligned, but ten sites mutated
  `code` directly - two of them `code.remove(mark_idx)`, which runs for every
  plain fixed-arg call, so the tables drifted on essentially any real script.
  Stack traces, `host_print` lines, and the serialized snapshot line table were
  all wrong. Emitted bytecode is byte-identical at every site.
- `next` distinguishes end-of-iteration from an invalid control key. Returning
  `(nil, nil)` for both meant `next(t, 0/0)` on a map-backed table reached
  `IndexMap::get_index_of`, whose hash hard-asserts `!is_nan()` - and that
  assert fires in release, so a script could abort the host. A control key that
  is not present now raises Lua's "invalid key to 'next'" instead of ending
  iteration silently.
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
  `0x1.8p+0`, NaN is `(0/0)`, inf is `1e9999`; the lexer's hex-float support
  above makes that output re-parseable by dellingr). `%p` stays deterministic:
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
