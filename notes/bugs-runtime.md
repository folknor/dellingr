# Runtime findings: VM dispatch, heap, tables, interrupts, standard library

Filed from the 2026-11-04 defect hunt (scopes `compiler-back`, `vm-dispatch`,
`vm-heap`, `stdlib-strings`, `stdlib-core`). Hunt reports are work documents
and may be wrong; verify before acting.

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.

The performance entries below (RT-015 through RT-020, RT-022) describe work the
hunters saw in the code. None has been measured; any change goes through a
registered `brokkr dellingr` workload first.

## RT-015 - Per-collection work scales with loaded code, not with garbage

Scope: `vm-heap`. Performance and structure.

Every collection walks every cache slot of every live runtime
(`drop_dead_cache_entries`) and walks the whole object slot map a second time
(`reachable_lua_bytecodes`). With many loaded chunks and small heaps, that work
can dominate a collection. The heap is a single non-incremental mark-sweep
triggered by allocation count, so one large table counts as one allocation
(documented in `docs/limits.md`).

Approaches raised so far (not exhaustive): marking runtimes reachable during the
mark phase (closures already visit `closure.runtime`) and pruning only those,
folding the second walk into the first.

## RT-016 - State::pop_n and balance_stack move one value per loop turn

Scope: `vm-dispatch`. Performance.

`State::pop_n` pops one value per loop turn, and `balance_stack` pushes one nil
per turn. `truncate` and `resize` express the same operations as single calls.

## RT-017 - sequence_len computes the raw border before checking for __len

Scope: `stdlib-core`. Performance and clarity.

`sequence_len` (`table.rs`) computes the raw border before checking for `__len`
and discards it when a handler exists, so every metamethod-path call does that
work for nothing. The expression `i64::try_from(..).map_or(i64::MAX,
i64::from)` there and in `table_sort` is an identity map written indirectly.

## RT-018 - gmatch construction re-interns strings already on the stack

Scope: `stdlib-strings`. Performance.

`gmatch` copies `s` and `pattern` into `Vec`s, then `push_bytes(&s)` re-interns
them, hashing up to 16 MiB to find the string that is already at stack slot 1 or
2.

Approaches raised so far (not exhaustive): when the argument is a string, push
the existing stack value (`push_value(1)`) instead of re-interning.

## RT-019 - str_match builds a fresh MatchState for every start position

Scope: `stdlib-strings`. Performance.

`str_match` builds a new `MatchState` for each start position, which initialises
a 32-entry capture array (about 512 bytes). A failed search over an n-byte
subject does this n times. The hunter observed that every path writes a capture
slot before reading it and that `captures()` reads only slots the successful
path set, and that the `SharedPattern` doc requires state to be fresh per
search, not per start position.

Approaches raised so far (not exhaustive): build the state once per search and
reuse it across start positions.

## RT-020 - string.format appends literal bytes one at a time

Scope: `stdlib-strings`. Performance. Raised by the hunt and again by a fix
wave.

Each literal byte of a format string goes through `append_output`, which runs
`checked_string_growth` and an atomic `state.charge` of 1 per byte. `%%` goes
through `parse_directive`, `format_argument` and a `Vec` allocation to emit one
byte.

Approaches raised so far (not exhaustive): batch literal runs with
`charge_units`, which admits charges exactly as per-byte charging does (the
technique `append_bytes_units` uses in gsub).

## RT-022 - Subject and pattern are copied on every pattern call

Scope: `stdlib-strings`. Performance. The code documents the copy as
deliberate.

`string_or_number_bytes` copies the subject and pattern on every `find`,
`match`, `gsub` and `gmatch` call. Its doc comment calls the copy deliberate and
names shared ownership of heap string bytes (an `Rc<[u8]>`-style handle) as the
way to remove it. `string.format` already borrows plain string arguments since a
fix wave.

Approaches raised so far (not exhaustive): the shared-ownership handle the doc
comment names, which would let `find`, `match` and `gsub` borrow.
