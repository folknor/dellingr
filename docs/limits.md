# Limits

All limits are build-wide constants in `dellingr::limits`. They are not
per-State settings: scripts must behave identically for every embedder and
across save/restore. Read the constants for current values. Limit errors carry
both the observed size or depth and the active limit.

## Source limits

Violations are reported as errors at load time, never silently truncated.

- `MAX_SOURCE_BYTES`: source text per load. Longer source is rejected with
  `SourceSizeExceeded` before tokenising.
- `MAX_SYNTAX_DEPTH`: budget of active recursive parser entries. When it is
  exhausted the parser raises `chunk has too many syntax levels`.
  - It is a recursion budget, not a source nesting depth.
  - A nested parenthesis consumes two entries (expression and unary parsers).
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
- `MAX_CALL_DEPTH`: simultaneously active Lua calls.
- `MAX_STACK_SIZE`: values on the shared Lua/Rust value stack.
- `MAX_METAMETHOD_DEPTH`: active steps in an index, newindex or
  non-function call chain. Table-library writes through metamethods share it.
- `MAX_ARGUMENTS`, `MAX_RESULTS`: values carried by one call or return.
  See below.
- `GMATCH_PATTERN_CACHE_ENTRIES`, `GMATCH_PATTERN_CACHE_BYTES`: per-State
  `string.gmatch` pattern cache.
- `COST_CHECK_INTERVAL`: static cost units batched before the budget is
  checked.

## Garbage collection policy

The heap counts GC-managed objects plus distinct interned strings. It checks
that count before each heap allocation and collects when the count is at least
the current threshold. The default automatic policy is an initial threshold of
20 allocations, a minimum threshold of 20, and a growth factor of 2. After an
automatic collection, the next threshold is
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

## Value stack

- Every path that can grow the stack is checked, batch-preflighted, or
  net-neutral. Exceeding the cap produces a catchable `StackOverflow` error.
- A rejected operation leaves the stack, the string pool and the function
  registry unchanged.
- The host methods `push_*`, `new_table`, `get_global` and `open_libs` are
  fallible for this reason.
- The cap bounds the shared value stack, not total host memory. Enforcing it
  charges no cost.

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
- Embedders must provision and validate thread stack space for their target,
  build profile and callback call paths.
- Debug builds use more native stack per call than optimized builds.
- The deep-recursion regression test runs on a dedicated large thread stack.
  That is a tested configuration, not a portable minimum.
- The syntax-depth bound exists for the same reason on the parse side: without
  it, deeply nested source would overflow the native stack.
