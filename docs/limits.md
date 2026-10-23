# Limits

Fixed resource, encoding, and reference-pinned library limits are constants in
`dellingr::limits`. They have no per-State override, so scripts behave the same
for every embedder and across save/restore. Limit errors carry the observed
size or depth and the active limit when they include numeric details. The GC
policy is a separate per-State setting, described below.

## Source limits

Violations are reported as errors at load time, never silently truncated.

- `MAX_SOURCE_BYTES`: source text per load. Longer source is rejected with
  `SourceSizeExceeded` before tokenising.
- `MAX_SYNTAX_DEPTH`: budget, in units, charged by active recursive parsers.
  When it is exhausted the parser raises `chunk has too many syntax levels`.
  - It is a recursion budget, not a source nesting depth. The statement,
    `if`, expression and unary parsers each charge one unit while active.
  - `SYNTAX_SUFFIX_COST`: extra units charged while the contents of a call,
    index or method-call suffix are parsed. It keeps native stack per unit
    below the parenthesis cycle in debug builds.
  - Units added per nesting level, by construct (the root statement list
    charges one more, and the innermost leaf needs its own headroom):

    | Construct | Units per level |
    |---|---|
    | parenthesis, `(x)` | 2 |
    | table constructor in a field, `{ {x} }` | 2 |
    | call argument or index, `f(f(x))`, `a[a[x]]` | 2 + `SYNTAX_SUFFIX_COST` |
    | unary operator, `- - x` | 1 |
    | right operand of `^` | 1 |
    | `if` body | 2 |
    | `do`, loop or named `function` body | 1 |

    So `local x = ((...1...))` compiles up to 98 parentheses and
    `return f(f(...f()...))` up to 49 calls.
  - `elseif` arms and expression suffixes such as `a.b[c]:m()()` are parsed
    iteratively. Their length consumes nothing; their contents still do.
- `MAX_CHUNK_NESTING`: function nesting depth, root included. It shares the
  numeric budget of `MAX_SYNTAX_DEPTH`, so nested functions hit it at the same
  boundary. Both raise `TooManySyntaxLevels`.
- `MAX_LOCALS`, `MAX_UPVALUES`: per function.
- `MAX_ASSIGNMENT_TARGETS`: 255 targets per assignment. The first excess target
  raises `TooManyAssignmentTargets` with its count and the limit.
- `MAX_REGISTERS`: 65534 registers per function, including temporaries.
  Exceeding it raises `TooManyRegisters` with the requested count and the limit.
- `MAX_NESTED`: direct child functions per chunk.
- `MAX_LITERALS`: distinct entries per numeric pool and per string pool.
- `MAX_FIXED_ARGS`, `MAX_FIXED_RESULTS`: compiler limits for fixed call
  arguments, call results and expanded assignments. `MAX_FIXED_ARGS` remains
  one below `MAX_ARGUMENTS`, leaving room for a `__call` receiver. The fixed
  result cap remains one below `MAX_RESULTS` to preserve the established
  accepted-program set. `OPEN_COUNT` (`u16::MAX`) is the open-count marker; the
  `u8` maximum remains an ordinary fixed count. A `RETURN` may still list
  values up to `MAX_RESULTS`.
- `MAX_EXPRESSIONS`, `MAX_ASSIGNMENT_OFFSET`, `MAX_FRAME_SLOTS`: fixed
  per-construct and per-function limits the compiler enforces.
  `MAX_ASSIGNMENT_OFFSET` is enforced through the assignment target cap, which
  a compile-time check keeps within it, so no accepted target list can exceed
  it.
- `MAX_TABLE_TEMPLATES`, `MAX_TABLE_TEMPLATE_FIELDS`: per-chunk constructor
  templates and the fields in each template. Constructors past the template
  limit use ordinary table construction. Field assignments have a separate
  cache-slot capacity; sites past it use the uncached path and are not
  rejected.
- `MAX_TABLE_BATCH_SIZE`: array entries per constructor batch. Constructors
  have no overall entry limit.
- Jump distance: a branch or loop may span fewer than 2^23 instructions of
  one function's bytecode; a longer one fails to compile with `JumpTooFar`.

## Runtime limits

- `MAX_STRING_BYTES`: size of any Lua string.
- `MAX_CALL_DEPTH`: simultaneously active Lua function calls. Only Lua
  frames count, and Lua-to-Lua calls hold their frames on the heap, so
  reaching it needs no native stack per level.
- `MAX_NATIVE_CALL_DEPTH`: simultaneously active calls made from Rust, which
  are the calls that run on the native stack. See "Native stack" below.
  Exceeding it raises `NativeCallDepthExceeded`.
- `MAX_STACK_SIZE`: values on the shared Lua/Rust value stack.
- `MAX_METAMETHOD_DEPTH`: active steps in an index, newindex or
  non-function call chain. Table-library writes through metamethods share it.
- `MAX_ARGUMENTS`, `MAX_RESULTS`: values carried by one call or return.
  See below.

## Reference-pinned library limits

These values preserve the current Lua library behavior and error paths:

- `MAXCCALLS`: pattern matching raises `pattern too complex` when recursive
  matching reaches 200 active calls.
- `LUA_MAXCAPTURES`: patterns allow 32 captures, including position captures.
  The next capture raises `too many captures`.
- `MAX_FORMAT_LEN`: one `string.format` directive is limited to 22 bytes. This
  follows Lua 5.4 `lstrlib`'s `MAX_FORMAT - 10` bound and raises
  `FormatDirectiveTooLong` when exceeded.
- `MAX_FORMAT_FIELD_DIGITS`: width and precision each accept at most two
  decimal digits. More digits raise `InvalidFormatSpecification`.
- `TONUMBER_BASE_MIN`, `TONUMBER_BASE_MAX`: `tonumber(value, base)` accepts
  bases from 2 through 36. Other values raise `BaseOutOfRange`.
- `TABLE_SORT_MAX_LEN`: `table.sort` rejects a length at or above `i32::MAX`
  (Lua's `INT_MAX`) with `ArrayTooBig`. `table.insert` and `table.remove`
  through sequence metamethods reject a shift distance at or above the same
  limit with `SequenceShiftTooLarge`, which names the function, before any
  shift handler runs.

## Compatibility exports for implementation tuning

`PATTERN_CACHE_ENTRIES`, `PATTERN_CACHE_BYTES`, and `COST_CHECK_INTERVAL`
are available through `dellingr::limits`. The cache values bound the per-State
compiled-pattern cache shared by `string.find`, `string.match`, `string.gsub`
and `string.gmatch`.
`COST_CHECK_INTERVAL` is the internal opcode-cost batching interval. These are
implementation tuning values, not additional script-level resource limits.

## Garbage collection policy

The heap counts GC-managed objects plus distinct interned strings. It checks
that count before each heap allocation and collects when the count is at least
the current threshold. The default automatic policy is an initial threshold of
20 allocations, a minimum threshold of 20, and a growth factor of 2. Each State
stores its own policy; these are the defaults used when a State is created.
After an automatic collection, the next threshold is
`max(live_allocations * growth_factor, minimum_threshold)`, using saturating
multiplication.

Embedders can provide per-State values with
`State::with_gc_policy(initial_threshold, minimum_threshold, growth_factor)`
or `State::with_callbacks_and_gc_policy`. The initial threshold is in force
while `open_libs` initializes the standard library. `gc_set_policy` replaces
the policy and restarts automatic mode at its initial threshold.

`gc_set_threshold` installs a fixed host threshold that remains in force
across explicit and automatic collections. `gc_set_auto_threshold` resumes
automatic mode at the policy's initial threshold. `gc_disable_auto` or a
threshold of `usize::MAX` disables automatic collection; explicit
`gc_collect` calls still work.

A running Lua function's registers are roots. A register whose value is no
longer used keeps that value reachable until the function overwrites the
register or returns.

GC policy is State-local and changes collection timing. Collection timing
affects when dead heap objects and interned strings are reclaimed, and can
affect rendered identities and snapshot contents. Keep the policy consistent
across replay runs.

## State-local growth

Beyond the limits above, these per-State structures have no fixed cap, and
no charge tracks their size. Their slot and map storage keeps its high-water
capacity; what a collection frees is the payloads and entries it removes.

- Reclaimed only by a collection, so with automatic collection disabled they
  grow until an explicit `gc_collect`:
  - dead heap objects and interned strings;
  - the runtime bundle (interned literals, inline caches) of each chunk that
    has been instantiated as a closure. A bundle also pins its chunk's
    bytecode, including nested chunks. Its literals are marked before unused
    bundles are dropped, so they are reclaimed one collection later;
  - the identity table behind rendered `tostring` and `%p` ids;
  - upvalue slots, which return to the pool's free list only at a
    collection: creating and discarding capturing closures grows the pool
    even when the number of live closures stays constant.
- Reclaimed only by the host:
  - anchors stay registered, and rooted, while their `Anchor` handle lives.
    `release_anchor` frees the slot at once; dropping the handle frees it at
    the State's next collection or anchor creation;
  - each `with_restricted_env` nesting level retains its environment until
    the call returns.
- Reclaimed only by writes to the table, never by a collection: a live
  table's storage. The array part stores a window from its first live key
  to its last. An append drops a dead prefix once it makes up half the
  window, so a FIFO (`q[tail] = v; q[head] = nil`) keeps storage
  proportional to its backlog, after catching up within a bounded number of
  pushes when it starts from a large dead prefix. A window whose live slots
  have all been cleared, in any order, is released at the next append or
  array write outside it. The window cannot skip
  holes: one old key that stays alive keeps every slot from it to the
  newest key stored, and nil writes alone never release anything. The hash
  part compacts its tombstones once they reach its live count. Until then a
  tombstone keeps its key alive, so a cleared key's string or object
  survives collections. A collection does not compact a live table, so one
  rooted table can retain large storage while counting as one heap object.
- A table's logical array length (the keys `#` and placement treat as array
  keys) never shrinks. It is capped at `min(2^53, 2^(pointer bits - 1)) - 2`
  keys; growing past it returns `table growth failed`, and a save beyond it
  is rejected on load. Array storage reservations are fallible too: an
  impossible gap in an edited save and an allocator refusal while growing a
  legitimate table both return that ordinary error and terminate the callback.
  This does not impose a smaller logical-length limit or discard live values.
  It does not make unrelated VM allocations fallible or protect against an
  operating system killing the process after accepting an allocation.
- Never reclaimed for the life of the environment: a global binding written
  before `State::seal_globals` keeps its slot after being set to nil, because a
  load's setup may reinstall that name and the saved nil is what keeps it
  deleted. Until the host seals, that covers every global, so a script that
  writes computed names through `_G` grows the bindings without bound. After
  sealing, bindings first created by later writes are reclaimed: once their
  nil tombstones make up half the bindings, the next new name compacts them.
  The map's allocated capacity stays at its peak. Bindings created inside
  `with_restricted_env` go away with that environment.
- Never reclaimed: with the `snapshot` feature, Rust function registrations
  persist while unreachable, one per registered id.

## Value stack

- Every path that can grow the stack is checked, batch-preflighted, or
  net-neutral. Exceeding the cap produces a catchable `StackOverflow` error.
- A rejected operation leaves the stack, the string pool and the function
  registry unchanged.
- The host methods `push_*`, `new_table`, `get_global` and `open_libs` are
  fallible for this reason.
- A Lua call reserves its function's whole register frame when it is
  admitted. A call whose frame does not fit under the cap fails there with
  `StackOverflow`, before its body runs, even when the path it would take
  needs fewer registers. A call from Lua reports that refusal at the
  caller's call instruction; the callee was never entered and is absent
  from the traceback. A fixed number of results then lands in registers
  that already exist, so such a return into a Lua caller never grows the
  stack.
- The cap bounds the shared value stack, not total host memory. Enforcing it
  charges no cost.
- A stack at the cap holds about 16 MB of values on a 64-bit host. Tests that
  push to `MAX_STACK_SIZE` pay that per State, inside the fixed virtual-memory
  cap `scripts/test_limited.sh` applies (`TEST_LIMIT_KB` in `scripts/lib.sh`).

## Host table methods

`get_table`, `get_table_raw`, `set_table_raw`, `table_next`,
`set_metatable_of`, `table_insert_at` and `table_remove_at` validate before
they pop. Every rejection detectable up front leaves the stack as it was:
bad index, wrong type, invalid key, invalid position.

Exception: when `get_table` calls an `__index` function and that call fails,
the key has already been consumed.

## Runtime call width

Calls and returns carry at most `MAX_ARGUMENTS` / `MAX_RESULTS` values. Both
are `u8`-wide because `ArgCount::Fixed`, `RetCount::Fixed` and the `RustFunc`
return value are `u8`; widening them would change those public APIs.

The cap applies to Lua functions as well as Rust callbacks:

- `return table.unpack(t)` or `return ...` with more values raises a
  too-many-results error;
- `f(table.unpack(t))` or `f(...)` with more values raises a
  too-many-arguments error.

## Native stack

Every recursion limit is a logical count, identical on every host, build
profile and thread size; none depends on how much native stack is left.

- A Lua function calling a Lua function, directly or through a `__call`
  handler, does not recurse natively: the dispatch loop suspends the caller
  in the State's activation records and continues in the callee.
- Every call made from Rust runs on the native stack: host `call` and
  `call_anchor`, metamethod handlers, generic-for iterator functions, and
  library callbacks such as `table.sort` comparators and `string.gsub`
  replacement functions. Each starts a nested dispatch loop or Rust callback,
  and `MAX_NATIVE_CALL_DEPTH` bounds how many can be active at once. The
  host's own call is one of them, and so is a Rust callback calling another
  Rust function through `State::call`.
- Other native recursion has its own bound: `__index` and `__newindex` table
  chains (`MAX_METAMETHOD_DEPTH`), the parser (`MAX_SYNTAX_DEPTH`) and the
  pattern matcher (`MAXCCALLS`).
- The counters are per State. A callback that runs another State nests that
  State's whole stack use on top of this one's, which is the embedder's to
  provision.

### Envelope

The runtime limits are sized so that all of them at once fit:

- a **2 MiB** thread, the Rust default for spawned threads, in this
  repository's release build settings;
- a **4 MiB** thread in its dev (unoptimized) build settings, whose frames
  are several times larger.

"At once" means: the host call plus nested calls made from Rust up to
`MAX_NATIVE_CALL_DEPTH`, through any of them (sort comparators, `gsub`
replacement functions and replacement tables, metamethod handlers, table
library reads, writes and lengths through metamethods, `tostring`, `print`,
`string.format` and `error` rendering a value, iterator functions, a
callback calling the host API), with an `__index` or `__newindex` table
chain held across them so the metamethod depth is at `MAX_METAMETHOD_DEPTH`,
and at the deepest point any one of:

- a pattern match one call short of `MAXCCALLS` through each recursive item
  kind (optional, greedy and minimal repetition), or its rejection at
  `MAXCCALLS`;
- a collection with a chunk tree nested to `MAX_CHUNK_NESTING` alive;
- `string.gmatch` creating its iterator in the last native level (the first
  call in a process also compiles the fixed wrapper chunk there), or refused
  past it;
- the refusal of the next call, through the same path as the levels below
  it, or of the next metamethod step, with its traceback;
- instead of a held chain, a string `__index` chain (the string metatable's
  `__index` naming a string, alone or alternating with a table) that uses
  the remaining metamethod depth and is refused.

These deepest-point items are separate peaks, each measured on its own, not
simultaneously live.

How `tests/native_stack.rs` validates it, on x86-64 Linux:

- The native stack a level costs is the frames on the compiled path that
  made the call, so the unit of coverage is the path, not the call site.
  Every path that can call back into Lua is listed, with its edges, in
  `reference/native-calls.toml`; reference/native-calls.md describes the
  guard that keeps that list current. The test runs each path on its own,
  repeated up to the native limit (callthrough levels fill any remainder a
  path's cycle leaves), with every held chain kind and every deepest-point
  item.
- Mixtures of paths need no runs of their own. A level of path `i` costs
  `c_i` bytes and holds `m_i` metamethod units that a chain step would
  otherwise spend at `s` bytes each, so the stack a mixture uses grows with
  each level's `c_i - m_i * s`, and the worst mixture for a chain kind is the
  homogeneous run of the path that maximizes it, which the test runs.
- A Rust function used as a generic-for iterator runs without a native
  level; the calls it makes into Lua are the levels (`ipairs` reading
  through `__index`, a host iterator calling `State::call`), and both are
  paths.
- A `__call` chain through non-function values is resolved in a loop: each
  step holds a metamethod unit while the call runs but adds no native frame,
  so it is not a chain kind of its own.
- It runs in both settings, each with and without the `snapshot` feature.
  The `snapshot` runs validate that build configuration only.
- The recurring cost per level is what its on-demand
  `native_bytes_per_level` probe measures. Transient peaks (descent, return,
  error construction, unwinding) are covered by the full-depth runs, not by
  the probe.

The 2026-10-23 x86-64 Linux measurement, comparing 97973d1 with the
22a263a fixes and an explicitly inlined invocation driver, records the full
scenario and frame tables in [the native-call reference](../reference/native-calls.md#measured-native-stack-2026-10-23).
The largest recurring cycle changes from 15,344 to 15,600 bytes in dev
(also with snapshot), from 3,824 to 3,776 in release, and from 3,872 to
3,840 in release with snapshot (3,824 after the shared vararg arena
follow-up recorded there, which leaves dev and plain release unchanged).
The largest table-chain step is 1,232
bytes in dev and 208 in release. A deliberately loose bound of 150 largest
cycles plus 200 largest chain steps is about 2.47 MiB in dev and 0.59 MiB
in release with snapshot, leaving about 1.53 and 1.41 MiB respectively
for fixed frames and transient work. This is recurring-stack headroom,
not a measurement of the transient peak; the unchanged full-depth tests
validate those peaks on the stated threads.

The release sort callback retains 32 bytes of frame growth for fallible
array and root-copy allocation. The dispatch frame is 32 bytes smaller,
and inlining the invocation driver into both call variants recovers the
extra 128 bytes per ordinary release cycle introduced when the generic
call path caused the compiler to outline it. Dev retains growth from the
fallible table operations, entry observation and result-copy path; the
two-native-level Rust-handler cycles grow more when the driver is inlined
into their Rust-only call arm. These changes fit the existing envelope;
neither the logical limits nor the test thread sizes change.

Parsing keeps the stricter promise: parsing at the syntax budget fits a
2 MiB thread in both settings.

What this means for an embedder: a debug build that runs scripts on a
thread it spawns with the default 2 MiB can overflow and abort the process
before a recursion limit is reached. Give such threads 4 MiB with
`std::thread::Builder::stack_size` (the main thread usually has 8 MiB
already). Other build configurations - a host's own optimization levels or
profile overrides for its dependencies, other targets - are the embedder's
to validate; the stated sizes are what this repository tests.

Outside the envelope, and the embedder's to provision:

- the host's own native frames below its first call into the State;
- what a Rust callback itself puts on the stack, per level of nesting;
- library work a callback starts at the deepest level besides the above,
  such as compiling source (`Program::compile`), which needs the
  parser's own stack on top of the nested calls;
- `hotpath`-instrumented builds, whose timing guards add to every level.
