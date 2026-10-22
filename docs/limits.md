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
- `MAX_NESTED`: direct child functions per chunk.
- `MAX_LITERALS`: distinct entries per numeric pool and per string pool.
- `MAX_FIXED_ARGS`, `MAX_FIXED_RESULTS`: fixed counts for calls and multiple
  assignment. The dynamic-count sentinel is reserved.
- `MAX_EXPRESSIONS`, `MAX_ASSIGNMENT_OFFSET`, `MAX_FRAME_SLOTS`: encoding
  limits of the bytecode operands.
- `MAX_TABLE_TEMPLATES`, `MAX_TABLE_TEMPLATE_FIELDS`: inline table templates.
  Field assignment sites past `MAX_TABLE_TEMPLATES` stop being inline-cached;
  they are not rejected.
- `MAX_TABLE_BATCH_SIZE`: array entries per constructor batch. Constructors
  have no overall entry limit.
- `MAX_VERIFIER_MARKER_DEPTH`: bytecode verifier marker stack depth.

## Runtime limits

- `MAX_STRING_BYTES`: size of any Lua string.
- `MAX_CALL_DEPTH`: simultaneously active Lua function calls. Only Lua
  frames count: a Rust callback that calls another Rust function through
  the host API recurses without consuming this budget, so it does not bound
  all calls. See "Native stack" below.
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
  (Lua's `INT_MAX`) with `ArrayTooBig`.

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
  window, and an all-dead window is released at the next append or array
  write outside it, so a FIFO (`q[tail] = v; q[head] = nil`) keeps storage
  proportional to its backlog, after catching up within a bounded number of
  pushes when it starts from a large dead prefix. The window cannot skip
  holes: one old key that stays alive keeps every slot from it to the
  newest key stored, and nil writes alone never release anything. The hash
  part compacts its tombstones once they reach its live count. A collection
  does not compact a live table, so one rooted table can retain large
  storage while counting as one heap object.
- A table's logical array length (the keys `#` and placement treat as array
  keys) never shrinks. It is capped at `min(2^53, 2^(pointer bits - 1)) - 2`
  keys; growing past it panics, and a save beyond it is rejected on load.
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

Lua calls recurse on the host thread's native stack.

- `MAX_CALL_DEPTH` and `MAX_METAMETHOD_DEPTH` bound logical recursion. They
  cannot guarantee that a small native stack reaches those limits before a
  process-aborting stack overflow.
- Rust callbacks that call Rust functions through the host API, directly or
  mutually, recurse with no VM-side depth limit. Bounding that recursion is
  the embedder's responsibility.
- Embedders must provision and validate thread stack space for their target,
  build profile and callback call paths.
- Debug builds use more native stack per call than optimized builds.
- The deep-recursion regression test runs on a dedicated large thread stack.
  That is a tested configuration, not a portable minimum.
- The syntax-depth bound exists for the same reason on the parse side: without
  it, deeply nested source would overflow the native stack.
