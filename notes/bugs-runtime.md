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

## RT-001 - Verifier-admitted bytecode can panic the VM at FOR_LOOP

Scope: `compiler-back` (the hunter followed it into `src/vm/eval_control.rs`).

`State::instr_for_loop` reads the three hidden numeric-for registers (control,
limit, step) with `.as_num().expect("numeric for variable stays numeric after
setup")`. The invariant behind that `expect` is a codegen property: `FOR_PREP`
writes the registers, nothing writes them afterwards, and no jump enters the
body without passing through `FOR_PREP`. The verifier checks none of it; for
`FOR_LOOP` it checks only `check_registers(pc, a, 4, frame)` and the jump
target.

Claims broken:

- The dispatch-policy comment above `validate_bytecode` in `verify.rs` says the
  verifier does not prove runtime value types and lists "numeric loop values"
  among what "still need fallible runtime checks". `FOR_LOOP` performs no
  fallible check; it panics.
- `docs/save-state.md`: "Saves are user-editable input." Snapshot bytecode is
  admitted by the same `validate_bytecode` (`vm/save_state/verify.rs`), so
  untrusted input reaches the panic. `Table::init_at_index` and
  `Table::with_template_keys` already treat forged snapshot bytecode as a case
  that must stay sound; this path is the exception.
- `frame.rs` accepts a panic only as "a verifier regression". The verifier never
  claimed this property, and the quoted comment says the opposite.

Minimal admitted chunk: `code = [FOR_LOOP 0 +0, RETURN 0 0]`, `max_stack = 4`,
`line_info` of length 2. Registers 0..4 are in the frame, the target (1) is in
range, and the last instruction is RETURN, so every check passes. Frame
admission sets the registers to nil and the first `FOR_LOOP` panics. Under
unwind that poisons the State (`reference/error-reporting.md`, "Panics and
poisoning"); under `panic = "abort"` it kills the host.

Other admitted shapes reaching the same panic: a `JMP` into the loop body that
skips `FOR_PREP`; a body instruction whose destination is in `a..a+2`; an open
producer (`VARARG`/`CALL` with all results) at a register below `a` whose values
spill over the hidden registers.

Compiled code is not affected: the hidden registers sit below `nactive`, and
every producer in a body sits at `freereg >= base + 4`.

Fix options the hunter gave:

- Make `instr_for_loop` fallible on a non-number, returning an
  `InternalFault`-style error as `reg_set_list` does for
  `ConstructorTableMissing`. The variant check already runs; the cost is a cold
  error arm. The hunter recommends this, since it matches what the verifier
  comment already says the VM does.
- Have the verifier prove the pairing: each `FOR_LOOP` targets `FOR_PREP + 1`
  with the same `A`, and that `FOR_PREP` exits to `FOR_LOOP + 1`. A sound proof
  must also exclude every write to `a..a+2` inside the region, including
  open-producer spills, which is much more verifier code.

## RT-002 - A Rust call is not a safepoint, so the documented bound on work between safepoints is false

Scope: `vm-dispatch`. See also RT-003, which breaks the same sentence through a
different mechanism.

Claim broken: the `src/vm/interrupt.rs` module doc says "Between two safepoints
the work is at most one linear pass over a string capped by `MAX_STRING_BYTES`
or over a table's storage." `docs/budget.md` ("Stopping a script from the host",
Safepoints bullet) repeats it.

The safepoints are backward jumps (`Frame::branch`, `instr_for_loop`), Lua
admission (`admit_lua_call`) and the library loops listed in `interrupt.rs`.
Calling a Rust function is not among them: `call_from_lua`, `begin_call`,
`begin_callable_call`, `invoke_rust_fn` and `instr_tfor_call_rust_fn` never
poll. Straight-line bytecode can therefore chain any number of Rust calls, each
a full linear pass, with no poll between them. Example: after building a 16 MiB
string `s`, a generated chunk consisting only of `s:upper()` lines (about 1.6
million calls fit in `MAX_SOURCE_BYTES` of 16 MiB) makes a 16 MiB pass, an
allocation and an intern hash per call without one poll. The same applies to
repeated `string.lower`, `reverse`, `sub`, `format`, and to Rust `__index`,
`__newindex` and `__len` handlers reached from straight-line code.

Work stays bounded by code size, so "no unbounded execution" holds; the "at most
one linear pass" bound does not. With no budget (the count-only default and the
CLI default) or under `Costing::Off`, an editor's stop button cannot stop such a
script for a long time.

A smaller instance of the same claim: an allocation-triggered collection
(`State::allocator` calls `maybe_collect`) is a pass over the whole heap, which
is neither a string nor one table's storage, and can happen between any two
safepoints.

Direction the hunter gave: poll once in `invoke_rust_fn` before calling `f`. It
is the single choke point for every Rust callee, the poll is one relaxed load,
and the interrupt would be reported at the caller's call instruction like an
admission refusal. Alternative: restate the bound as "per library call" and drop
the stronger wording.

## RT-003 - string.format can make about 254 full 16 MiB passes between two safepoints

Scope: `stdlib-strings`. Same claim as RT-002, different mechanism: this one is
inside a single library call, so polling at Rust-call entry does not fix it.

Claim broken: `src/vm/interrupt.rs` and `docs/budget.md`, the "at most one
linear pass" sentence quoted in RT-002. The `format` loop's own comment ("one
pass over the format string's bytes, each directive rendering one argument")
gives the per-directive bound, not the total.

`string.format` has no safepoint of its own; a directive polls only when it runs
a `__tostring` handler. Each argument directive makes a full pass over its
argument:

- `%s`: `tostring_bytes_unguarded` copies the whole rendering (`to_vec()`). With
  a precision, `bytes.truncate` drops it afterwards, so `%.0s` copies 16 MiB and
  appends nothing, and the output cap (`checked_string_growth`) never bounds it.
- `%q` of a string: `to_vec()` plus `quote_string`'s pass.
- `%d`/`%f`/... of a numeral string: `to_vec()` plus `parse_lua_numeral` (a
  valid numeral can be 16 MiB of whitespace around one digit).

With `MAX_ARGUMENTS` at 255, one call can make 254 such passes, about 4 GiB of
copying with no poll. Under a default model the charge (16M per `%s`) usually
stops this through the budget, but the interrupt contract holds whatever the
costing: `Costing::Off`, a zero `StringFormat` weight (the `budget.md` example
itself sets one) and an unset budget all reach it. Bounded work, so latency, not
a hang.

Fix the hunter gave: poll once per directive in the `format` loop (interrupts
charge nothing, so this is cost-neutral). Separately, render `%s` with a
precision by slicing before copying: keep charging the full length as today
("Charge the full rendering before precision truncation" stays true) but copy
only `min(precision, len)` bytes, and for a plain string with no `__tostring`
borrow the stack value instead of `to_vec()`. Shared ownership of heap string
bytes (RT-022) would remove the copying entirely.

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

## RT-005 - array_insert can fail its growth reservation after mutating the table

Scope: `vm-heap`.

Claims broken:

- `table.rs` `reserve_array`: "Growth paths call this before touching the
  logical length or removing migrated hash keys, so a refusal leaves the table
  logically unchanged."
- `table.rs` `reserve_outside_window`: "Reserve the storage
  `write_outside_window(key, ..)` will need once the window has grown by
  `extra` pushed slots."
- `docs/save-state.md`: an array growth that cannot be reserved "raises the
  ordinary `table growth failed` error, which kills the callback and leaves the
  table unchanged". `table_ops.rs` `table_insert_at` promises "leaves the stack
  and table unchanged" for its other rejections.

In `Table::array_insert`, the structural case (`len == vlen`) with
`pos <= base` and a live value reserves with `reserve_outside_window(pos,
extra)`, computed from the current `array_live`. It then pushes, rotates and
runs `migrate_from_hash`, which pulls `extra - 1` live values in and raises
`array_live`. The final `write_outside_window(pos, value)` calls
`reserve_outside_window(pos, 0)` again with the larger `array_live`, so its
downward padding `pad = min(pos - 1, array_live.div_ceil(2))` can exceed what
was reserved. If that second reservation needs more capacity and the allocator
refuses, `GrowthFailed` returns after `vlen` grew, elements shifted and hash
keys migrated, but before the value was written: `table.insert(t, pos, v)`
half-applied.

Concrete shape: keys 1..100 set, 1..60 cleared, append 101 (drops the dead
prefix: base 60, 41 live), live hash keys 103 and 104, then
`table.insert(t, 50, v)`: reserved pad 21, needed pad 22.

Practical impact is small (only an allocator refusal at exactly that point
exposes it; otherwise the extra growth succeeds unreserved), but it breaks the
reserve-then-mutate discipline, and the helper's own doc misleads by accounting
for the pushed slots' count but not that migrated slots are live. Fix: reserve
with the post-migration live count (`array_live + extra - 1` in the structural
case), or reserve in one helper taking both the pushed count and the live delta.

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

## RT-007 - unpack checks the key range before the result count, so an over-long range reports the wrong error

Scope: `stdlib-core`.

In `unpack_values` (`src/lua_std.rs`), `table::check_sequence_keys(i, j, ...)`
runs before the `span >= MAX_RESULTS` test. `table.unpack({}, 1, 2^60)` (or
`unpack(t, 1, 2^54)`) therefore raises `IntegerKeyRangeExceeded` ("unpack range
exceeds the exactly representable integer keys") instead of `TooManyResults`.
The real problem is that it asks for about 2^60 results; Lua 5.4 reports "too
many results to unpack", and dellingr does too when the range lies inside
+/-2^53.

Claim broken: `docs/compatibility.md` (Table library) says of the key-range
refusal that "the functions' other argument checks still apply first". The
result-count limit is applied second here. Also a misleading message for the
script author.

Fix: test `span >= MAX_RESULTS` (with the `checked_sub`) before
`check_sequence_keys`. A range of up to 255 keys reaching past 2^53 still gets
the key-range error, which is the case the refusal exists for.

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

## RT-009 - The safepoint-loops check does not scan src/lua_std.rs

Scope: `stdlib-core`.

Claims broken: `src/vm/interrupt.rs` (the `safepoint-loops` check "holds every
loop in the standard library ... to that") and AGENTS.md (it "makes every loop
in native library code poll or carry a ... comment").

`scripts/safepoint_loops.py` scopes itself with `SCOPE_DIRS = ("src/lua_std/",
"src/patterns/")`. The library's root module `src/lua_std.rs` does not start
with `src/lua_std/`, so it is never scanned. Loops there with neither a poll nor
a decision comment: the `for index in i..=j` element loop in `unpack_values`;
the `for` in `install_library_functions`; the `while` loops in the const fns
`segment_offset` and `position_in`. All are bounded today (unpack's span is
capped below `MAX_RESULTS`; the rest run over fixed lists or at compile time).
The gap is in the guarantee: a future unbounded loop in `lua_std.rs`, where the
shared argument helpers live, would pass `brokkr check`. The unpack loop's
comment explains the charge decision but carries no `bounded:`, and the check
accepts it.

Fix: add `src/lua_std.rs` to `SCOPE_FILES`, and add `// bounded:` comments to
those loops (unpack: "at most MAX_RESULTS elements, checked above").

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

Scope: `stdlib-strings`. Performance.

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
handle). With that, `find`, `match`, `gsub` and `format` (`%s`, `%q`, numeral
arguments) would be borrow-only, and RT-003's copying would go with it.
