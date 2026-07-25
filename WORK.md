# WORK.md

Current work item. Numbers refer to finding numbers in `notes/bugs.md`.

---

## Targets #30, #31, #53: save/load fidelity

Three snapshot defects. Unlike the previous loops these are **not** reference
conformance issues - Lua has no snapshots - so "correct" means a loaded State
behaves like the saved one. Verification is a round trip, not a diff against
5.2/5.4.

All three verified by reading; each needs a round-trip repro written as part of
the fix.

### #30 (Medium) - user mutations inside environment tables are silently dropped

`src/vm/save_state.rs:412-419`. When the encoder meets an `ObjectPtr` present
in `env_reverse` it emits an `EnvObj` token and **never walks the table's
entries**:

```rust
Val::Obj(ptr) => {
    if let Some(token) = self.env_reverse.get(&ptr) {
        Ok(SavedVal::EnvObj(token.clone()))   // <- entries never visited
    } else {
        let id = self.object_id(ptr, path, &mut tasks)?;
        Ok(SavedVal::Obj(id))
    }
}
```

On load those tokens resolve against freshly rebuilt pristine libraries
(`806-822`). So ordinary Lua idiom - extending a library table - vanishes:

```lua
math.myconst = 42
string.trim = function(s) return s end
table.helpers = { deep = "value" }
```

Everything reachable *only* through such a table is dropped entirely. No
`SaveError`, no diagnostic. The module doc only covers the reverse direction (a
new build adding `math.foo`), and the README's snapshot section does not carve
this out.

### #31 (Medium, now worse) - `%p` identity state is not persisted

`format_pointer_ids` and `next_format_pointer_id` (`src/vm.rs:181-182`) appear
nowhere in `save_state.rs`. After a load the counter restarts at 1.

**Loop 15 raised the stakes.** `tostring` on any object now renders
`table: 0x{id}` from this same registry, so this is no longer confined to
scripts that call `%p`. A pre-save `tostring(t)` result stored in a saved
global can be byte-identical to a *different* object's `tostring` after load,
which breaks the uniqueness the deterministic ids exist to provide, and makes
an uninterrupted run diverge from a save/load-interrupted one.

### #53 (Low) - anchors created in the `load_state` setup closure are dead

`load_state` runs `setup(&mut state)` (`784`) and then `materialize_payload`
(`785`), which ends with `state.registry.clear()` (`930`). A host that pushes a
value and anchors it during setup gets a handle that is stale the moment
`load_state` returns; later use returns `InvalidAnchor` with no error at
creation time.

The clear exists to drop anchors inherited from `State::with_callbacks`. A
freshly built State has none, so moving the clear earlier is free.

### Constraints

- **Determinism.** Save output must stay byte-identical for identical states -
  the encoder relies on `BTreeMap`s, traversal order and insertion-ordered
  globals. Any new persisted collection must preserve that.
- **Format version.** `FORMAT_VERSION` strict equality is the compat gate.
  Adding fields to `SavePayload` is a format break and the version must be
  bumped; the previous session already moved v2 to v3, so old saves are
  rejected rather than silently misread.
- Decoder hardening still applies: reservations are capped by remaining input
  so forged length prefixes cannot allocate unboundedly, and traversal is
  iterative so a deep decoded graph cannot overflow the stack.
- `validate_quiescent` must still hold after load.
- Charge nothing new (#16).

---

## Agreed implementation plan

Settled between the orchestrator and the deep reviewer.

### #30 - persist and replay environment deltas

Approach (a). Failing or merely diagnosing would leave ordinary library
extension unsavable, which contradicts the snapshot contract in
`README.md:73`.

**Detection by `Table::version()` is not viable** - the version deliberately
does not change on a value update (`table.rs:53`), so `math.floor = myfloor`
would evade it. Diff against a captured baseline instead.

1. **Capture the baseline** in `capture_env_tokens` (`src/vm.rs`): the ordered
   entries and metatable of each tokenized environment object. For the current
   standard environment that is five objects and 41 entries (math 22, string
   10, table 7, `_G` none directly, `_G`'s metatable 2).
2. **Root it.** Add the canonical objects and baseline `Val`s to
   `mark_gc_roots`. That function is the single source of truth for
   reachability and must stay so.
3. **Emit `SavedEnvDelta`** in lexical token order, containing:
   - deleted pristine keys - mandatory, or `math.floor = nil` is resurrected
     from the fresh library on load;
   - added or replaced key/value pairs, with their new values;
   - an optional final key order, only when delete/upsert replay would not
     reproduce `pairs()` order (`Table::entries()` is insertion-ordered,
     `table.rs:203`, and load deliberately rebuilds that order, `table.rs:220`);
   - a tri-state metatable change: unchanged, cleared, or set.

   Deletions in baseline order, upserts and order keys in live insertion order,
   so output stays byte-deterministic.
4. Keys and values are `SavedVal`, so a delta value may itself be an `EnvObj`.
   Schedule live delta values through the **existing iterative task walker**, so
   graphs reachable only through an env table - and cycles like
   `math.user = t; t.back = math` - land in the normal arenas without
   recursion.

Cost when nothing was modified: four bytes (one empty-vector length), an O(E)
scan of 41 entries, no traversal or value encoding.

### #31 - persist pointer identities

Persisting is the correct contract, not documenting them as per-process. These
are logical deterministic State identities, observable as strings and storable
by scripts; since loop 15 they back `tostring` for every table and closure, so
a per-process contract would make an interrupted run observably differ from an
uninterrupted one.

Add to `SavePayload`: `next_format_pointer_id: u64` and
`format_pointer_ids: Vec<(SavedVal, u64)>`.

- **Persist only reachable entries.** The map is deliberately *not* a GC root
  (`vm.rs:178`, absent from `mark_gc_roots`), so it can hold generational keys
  for already-collected objects. Treating it as a serialization root would
  change its lifetime contract and could try to encode stale values. Have
  `SaveBuilder` track pointer-like values reached through globals, user objects
  and referenced env tables, then filter the id vector against that set,
  preserving each surviving pair's explicit id and original order.
- **Save the exact counter regardless.** Ids are serialized explicitly, not
  derived from vector position, so dropping dead entries renumbers nothing -
  but if dead id 1 is dropped, the next object after load must still get 2, not
  reuse 1.

### #53 - move the registry clear

Move `registry.clear()` from the end of `materialize_payload` to immediately
after the fresh `State` is constructed, before `setup`. Free: both
`with_callbacks` and `empty_with_callbacks` build an empty `Registry`
(`vm.rs:270`, `anchor.rs:90`) and opening libraries creates no anchors.

Setup-created anchors must then **survive**, which is the point: the registry
is a GC root (`vm.rs:98`), so the final `gc_collect` would otherwise free
values held only by setup. Registry contents are not a quiescence violation
(`save_state.rs:789`), and anchors still remain deliberately unpersisted.

### Format version 4

Bump `FORMAT_VERSION` 3 -> 4 (`save_state.rs:56`). Mandatory: `SavePayload` is
positional and the reader gates on exact equality before reading anything else
(`save_state.rs:1140`). **No fallback parsing for v3.**

Decode every new collection through `read_vec` so the remaining-input
reservation cap still defangs forged length prefixes (`save_state.rs:1454`).
Extend payload verification to reject duplicate environment tokens, malformed
delta modes, duplicate pointer values or ids, and invalid referenced values.

### Tests

`tests/save_state.rs`: env additions containing nested data and closures; stock
deletion and replacement; env-to-user-data cycles and `EnvObj` delta values;
insertion order and env metatable changes; surviving pointer ids, next object
id, and a dropped-id counter hole; control run vs save/load run agreeing on
`tostring` and `%p`; an anchor created during load setup surviving the final
GC; patched older *and* newer version words returning `UnsupportedVersion`
before setup runs; and an immediate resave after load proving quiescence.

Add codec unit round trips for the new structures and extend the golden
program to exercise an environment delta and a pointer id.

**The golden fixture will need regenerating** (`tests/fixtures/save_golden.bin`)
because the format changes. Do not attempt it - the orchestrator runs
`brokkr test regenerate` after the build, since the ignored regeneration helper
needs cargo.

Read `src/vm/save_state.rs` (`SaveBuilder::encode`, `build_env_reverse`,
`materialize_payload`, `load_state`, `SavePayload`, `verify_payload`),
`src/vm.rs` (`capture_env_tokens`, the `format_pointer_ids` fields,
`mark_gc_roots`), `src/vm/table.rs` (`version`, `entries`), and
`tests/save_state.rs`.

---

## Review findings to fix

The above is implemented and `brokkr check` passes. Review then found five
defects. **Finding 1 is already fixed** by the orchestrator (`mark_gc_roots`
now calls `GcHeap::mark` instead of pushing onto the worklist, so canonical
environment objects are actually coloured and survive sweep). Fix the rest.

### A. Reachable string and Rust-function `%p` identities are dropped

`save_state.rs:358` filters the id list to `Val::Obj` only, and reachability
tracking is updated only for objects (`529`); `verify.rs:245` then rejects
anything but `Obj`/`EnvObj`. But `%p` assigns identities to **strings and all
functions** too (`string_format.rs:348`), so:

```lua
s = "reachable"
before = string.format("%p", s)
```

reloads with the right counter but no mapping for `s`, and formatting it again
yields a different id - the exact divergence #31 exists to prevent.

Persist identities for reachable strings and registered Rust functions as well,
using the `SavedVal` forms those already have (`Str`, `Fn`). Widen the verifier
to match. Extend the test, which currently covers only a table
(`tests/save_state.rs:551`).

Also close the related hole: an unchanged tokenized environment table's children
are never traversed, so `_G.metatable`'s id survives only if some other
serialized value happens to reference it directly.

### B. Ordered replay rejects later library or setup additions

Order replay copies only the saved keys (`save_state.rs:1043`) and then requires
that count to equal the whole current table (`1054`), so any fresh-build or
setup-added key that the old save never saw makes load fail with
`CorruptArena`. Reproducible in a single build: save a state whose `math`
mutations need an order vector, then add `math.future` in the load setup
closure.

Rebuild saved keys in saved order and **append current-only entries** in their
existing order. The plan explicitly required preserving later additions.

Second defect in the same path: `clear_and_insert_entries` replaces the whole
`Table` and so clears its metatable (`table.rs:220`). If a later library or
setup installed a metatable while the saved delta says `Unchanged`, ordered
replay silently drops it before the `Unchanged` arm runs (`1061`).

### C. Delta key verification is not runtime key equality

`verify.rs:184` compares `SavedVal` representations, which is not the same as
`Val` key equality:

- two `SavedVal::Str` indices can reference identical bytes, and materialization
  interns both to one string pointer (`save_state.rs:956`);
- `+0.0` and `-0.0` have different saved bits but compare equal as `Val::Num`
  (`lua_val.rs:141`).

Such keys pass the ordered sets and then collapse to a single table key.
Verification also currently permits nil and NaN keys, nil-valued upserts, order
keys overlapping deletions, and order vectors missing upsert keys.

Reject all of those. Compare keys by their **resolved** identity - resolve
`Str` indices to bytes and normalise `Num` zero - not by encoded form.

### D. Ordered replay is quadratic in attacker-controlled input

Each decoded order key linearly scans all current entries
(`save_state.rs:1046`), so a payload with N upserts and an N-key order vector
passes the O(N log N) verifier and then does O(N^2) comparisons during
materialization. Length prefixes cap allocation but not CPU.

Build a lookup keyed by resolved key identity once, then place entries in a
single pass. Note the orchestrator already converted the *verifier*'s duplicate
checks from linear scans to `BTreeSet`s for this same reason, and derived `Ord`
on `SavedVal` to allow it - reuse that where it helps.

### Constraints unchanged

Save output must stay byte-deterministic; `mark_gc_roots` remains the single
source of truth; all decoding stays behind `read_vec`. Add regression tests for
each of A-D, including the `math.future`-in-setup case for B.
