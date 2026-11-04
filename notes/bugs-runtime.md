# Runtime findings: VM dispatch, heap, tables, interrupts, standard library

Filed from the 2026-11-04 defect hunt (scopes `compiler-back`, `vm-dispatch`,
`vm-heap`, `stdlib-strings`, `stdlib-core`). Hunt reports are work documents
and may be wrong; verify before fixing.

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

## RT-004 - A cleared global does not keep its place in pairs(_G) once the sealed table compacts

Scope: `vm-heap`. Replay-visible order.

Claims broken:

- `docs/compatibility.md`, Globals and metatables: "In `pairs(_G)`, a global
  set to nil and later redefined keeps its earlier place, where an ordinary
  table moves a re-added key to the end."
- `docs/limits.md`, State-local growth: "Clearing and redefining the same global
  reuses its slot in every phase."
- `global_env.rs` `slot_policy`: "A global keeps its slot (and its place in
  `pairs(_G)`) however often it is cleared and set, as a binding always has".

After `seal_globals` the global table's policy is `revive_in_place: true,
retain_dead: false`. A tombstone revives in place only while its slot exists.
Inserting a new key goes through `Table::insert_into_map_with_tombstones`, which
compacts when `dead_count >= live_count` (`compact_dead`, permitted by the
sealed policy). Compaction removes the cleared global's slot, so redefining it
appends at the back.

Sequence (sealed): `a = 1; b = 1; a = nil; c = 1; a = 2` gives `pairs(_G)` order
`b, c, a` (after whatever setup installed), not `a, b, c`. Before sealing
(`retain_dead: true`) the claim holds.

Deterministic, so replays are not broken; three documents state an
unconditional guarantee about replay-visible order that holds only until the
next compaction. Options: qualify the docs ("after `seal_globals`, until a later
new global compacts the cleared slot"), or keep the global table's string-keyed
tombstones permanently (what `retain_dead` does before sealing, and what
limits.md says sealing exists to stop). The hunter marks the trade-off as the
user's call; the current wording is wrong either way.

## RT-006 - A saturated automatic GC threshold becomes permanently Disabled

Scope: `vm-heap`.

Claim broken: `docs/limits.md`, GC policy: "After an automatic collection, the
next threshold is `max(live_allocations * growth_factor, minimum_threshold)`,
using saturating multiplication." The `gc_set_policy` doc in `vm.rs` repeats it.

`GcHeap::collect` recomputes only while the threshold is
`GcThreshold::Automatic`, and builds the new one with
`GcThreshold::automatic(...)`, which maps `usize::MAX` to
`GcThreshold::Disabled`. A saturating product (large `growth_factor`) therefore
switches the State out of automatic mode. Every later collection, explicit
`gc_collect` included, skips recomputation, so when the live count drops the
threshold never returns to `max(live * growth, minimum)`. Only
`gc_set_auto_threshold`/`gc_set_policy` recovers.

The conflation is "automatic, threshold currently unreachable" versus "automatic
collection disabled by the host". Low severity (needs a huge growth factor), but
the documented formula is not what runs. Fix: keep `Automatic(usize::MAX)` as a
distinct state; only `initial_threshold == usize::MAX` and host calls produce
`Disabled`.

## RT-008 - error(msg, 1) and error(msg, nil) can render differently from error(msg)

Scope: `stdlib-core`.

`error` in `src/lua_std/basic.rs` maps an absent level to
`PrefixLocation::Current`, but an explicit level 1 or explicit `nil` to
`PrefixLocation::TraceFrame(0)`. `impl Display for Error` (`src/error.rs`)
treats them alike when the stack trace has a frame 0. With an empty trace they
differ: `Current` falls back to `source_name()`/`line_num`, while
`TraceFrame(_)` with no frame suppresses the prefix. A host calling the `error`
builtin directly (no Lua frame on the trace) gets a position prefix for
`error("x")` but not for `error("x", 1)` or `error("x", nil)`.

Claim broken: the function's own default. Both references treat `error(v)` and
`error(v, 1)` as one call (`luaL_optinteger(L, 2, 1)`), and dellingr's own code
reads an absent level as 1 a few lines later (`unwrap_or(1)`). Edge case.

Fix: map level 1, absent and nil to one `PrefixLocation`, either by folding
`TraceFrame(0)` into `Current` or by having the no-argument path produce
`TraceFrame(0)`.

## RT-010 - Lateral: global stores silently drop a write to a vacant cached slot

Scope: `vm-heap`. Not a defect today; latent hazard.

`GlobalEnv::write_cached` and `GlobalEnv::write` ignore `Table::write_slot`'s
`false` (vacant or missing slot) and report the store done. Unreachable now
because the global table revives in place and never creates `Vacant` keys, and
`hash_slot_of` never returns one. A future policy change, or a load path that
admits `(nil, nil)` slots into the global table, would turn a global store into
a silent no-op. Suggested: a `debug_assert!` on the return, or make
`write_slot` infallible for revive-in-place maps.

## RT-011 - Lateral: the release-mode upvalue pool is not defensive against double free

Scope: `vm-heap`. Latent; reachable only through a root-set bug.

`UpvaluePool::mark_slot` only `debug_assert!`s that a slot is not `Free`. In
release, marking a freed slot flips it to `Reachable`; the next sweep sets it to
`Unmarked` while it is still on the free list; a later sweep can push it a
second time, after which two allocations share one slot. The failure mode is
silent upvalue aliasing, unlike the heap's `get`, which fails loudly on a stale
key.

## RT-012 - Lateral: GcHeap::get_mut couples every mutable access to the builtin-mirror epoch

Scope: `vm-heap`. Correct today; structural smell.

Every mutable access to any object compares against `watched` and bumps the
global epoch, so the builtin mirror is invalidated by `_G.x = 1`,
`rawset(_G, ...)` or any table-library call on `_G`, even for non-builtin keys.
The coupling between a generic accessor and one table's cache lives in
`object.rs`, and the one bypass (`watched_table_unmarked`) relies on a `builtin`
bit the caller cached. A `GlobalEnv`-owned mutation API would make the epoch
rule local.

## RT-013 - Lateral: template tombstones are compacted by the first unrelated keyed insert

Scope: `vm-heap`. Correct result; possibly defeats the template's purpose.

`Table::with_template_keys` makes every template key a tombstone
(`dead_count == map.len()`), so the first `INIT_INDEX` or non-pinned insert into
a template table (live 0, dead n) compacts all reserved slots and bumps the
version; later `INIT_FIELD_PINNED` writes then miss `init_at_index` and fall back
to keyed inserts. That defeats templates for constructors that put computed keys
before named ones. The hunter suggests checking codegen's emission order.

## RT-014 - Lateral: inline-cache validation is re-implemented at every consumer

Scope: `vm-heap`. Structural.

The rule "same table, same version, slot live; otherwise re-read the key at the
cached index and refresh" exists separately in `get_cached_field`,
`get_current_cached_field`, `cached_method_at`, `get_cached_string_method` and
`try_set_field_cached`, with a global variant (`Present`/`Absent` plus slot
count) in `GlobalEnv::read_cached`/`write_cached`. Soundness rests on table.rs
invariants written only as prose: which operations bump the version, that
`Vacant(i)` always sits at index `i`, that inline keys are unique. A
`Table`-owned typed slot reference (`SlotRef { version, index }` with
`Table::read_slot(&SlotRef, key)` / `write_slot(..)`) would put the invariant
and its consumers in one file and collapse the five copies. Cache entries also
do not record their key; correctness rests on the verifier's one-site-per-slot
rule (`compiler/verify.rs`), which is checked but is a cross-module dependency
worth naming at the cache definitions.

## RT-015 - Lateral: per-collection cost scales with loaded code, not garbage

Scope: `vm-heap`. Performance and structure.

Every collection walks every cache slot of every live runtime
(`drop_dead_cache_entries`) and walks the whole object slot map a second time
(`reachable_lua_bytecodes`). With many loaded chunks and small heaps this
dominates. Marking runtimes reachable during the mark phase (closures already
visit `closure.runtime`) and pruning only those would fold the second walk into
the first. The hunter also notes that the heap is a single non-incremental
mark-sweep triggered by allocation count, so one large table counts as one
allocation (documented in limits.md).

## RT-016 - Lateral: State::pop_n and balance_stack work one value per turn

Scope: `vm-dispatch`. Performance.

`State::pop_n` pops one value per loop turn, and `balance_stack` pushes one nil
per turn. `truncate` and `resize` do the same work in one call.

## RT-017 - Lateral: sequence_len computes the raw border before checking for __len

Scope: `stdlib-core`. Performance and clarity.

`sequence_len` (`table.rs`) computes the raw border before checking for `__len`
and discards it when a handler exists: wasted work on every metamethod-path
call. `i64::try_from(..).map_or(i64::MAX, i64::from)` there and in `table_sort`
is an identity map with extra steps.

## RT-018 - Lateral: gmatch construction re-interns strings already on the stack

Scope: `stdlib-strings`. Performance.

`gmatch` copies `s` and `pattern` into `Vec`s, then `push_bytes(&s)` re-interns
them, hashing up to 16 MiB to find the string already at stack slot 1 or 2. When
the argument is a string, push the existing value (`push_value(1)`) instead.

## RT-019 - Lateral: str_match builds a fresh MatchState per start position

Scope: `stdlib-strings`. Performance.

`str_match` builds a fresh `MatchState` for every start position, initialising
the 32-entry capture array (about 512 bytes); a failed search over an n-byte
subject does n of these. Every path writes a capture slot before reading it, and
`captures()` reads only slots the successful path set, so the state can be built
once per search and reused, keeping "fresh per search" (the `SharedPattern` doc)
intact.

## RT-020 - Lateral: string.format appends literal bytes one at a time

Scope: `stdlib-strings`. Performance. The fixer that added the per-directive
poll to `string.format` re-raised this.

Each literal byte of the format string goes through `append_output`
(`checked_string_growth` plus an atomic `state.charge` of 1). Batch literal runs
with `charge_units`, which matches per-byte admission exactly (as
`append_bytes_units` does in gsub). `%%` also goes through `parse_directive`,
`format_argument` and a `Vec` allocation to emit one byte.

## RT-021 - Lateral: SplitMix64 seeds a gamma apart give overlapping streams

Scope: `stdlib-core`. Documentation suggestion, not a defect.

Seeds differing by a multiple of the gamma constant `0x9e3779b97f4a7c15` give
the same stream shifted by whole draws (seed `s + gamma` is seed `s` one draw
later). Acceptable under "not cryptographic", but a host deriving per-entity
seeds as `base + k * gamma` would get overlapping streams. A sentence in
`set_rng_seed`'s doc would head this off.

## RT-022 - Lateral: subject and pattern are copied on every pattern call

Scope: `stdlib-strings`. Performance; the code already names this as deliberate.

`string_or_number_bytes` copies the subject and pattern on every
`find`/`match`/`gsub`/`gmatch` call. Its doc comment calls this deliberate and
names the fix: shared ownership of heap string bytes (an `Rc<[u8]>`-style
handle). With that, `find`, `match` and `gsub` would be borrow-only.
`string.format` already borrows plain string arguments.

## RT-024 - Lateral: the raw table.move per-copy poll is now untested, and its test is misnamed

Raised during a fix wave by the green agent. Test coverage.

Rust callbacks now poll for an interrupt at entry, so a pending interrupt stops
`table.move` before it prepays or copies anything.
`host::interrupt::an_interrupted_raw_move_keeps_its_destination_and_prepaid_cost`
was adjusted to assert that (the run with the interrupt costs the same as the
one without), but its name still describes the old behaviour, and the raw
move's own per-copy poll can no longer be reached by a pending interrupt, so
nothing exercises it. A test that raises the interrupt mid-move (for example
from an `__index`/`__newindex` handler or a host callback invoked during the
copy) would cover it; rename the existing test to what it now checks.

## RT-023 - Lateral: string.format duplicates the %s rendering tail across two arms

Raised during a fix wave by the reconciler. Smell.

In `format_argument` (`src/lua_std/string_format.rs`), the `Itself` and `Bytes`
arms of the `%s` case are near-identical copies: the embedded-zero check, the
precision cut and the padding. One shared helper taking `&[u8]` would remove the
copy. The borrow of `state` across the arms is what makes merging them fiddly.
