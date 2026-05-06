# Anchor: a registry for retainable Lua values

## Problem

Embedders frequently need to retain a Lua function from one Rust callsite and call it later (event handlers, callbacks registered from script, anything fired from Rust). Today the only path is stashing it in a Lua global, or in a global-table-of-handlers the embedder manages by name:

- Stringly-typed keys, owned by the embedder.
- Visible to scripts using `with_restricted_env` (which swaps globals + builtins, `src/vm.rs:454-487`).
- Easily clobbered by accidental script-side reassignment.
- Doesn't survive collision with whatever names the script wants to use.

The reference C API solves this with the registry: `luaL_ref(L, LUA_REGISTRYINDEX)` consumes the top-of-stack, returns an opaque integer; `luaL_unref` releases it. Registry table itself is invisible to scripts.

## Recommended design

A per-`State` registry of `Val`s, keyed by a `Copy + Send` 96-bit handle backed by `slotmap::SlotMap`. One handle type for any `Val` shape. Generational stale-handle detection. Cross-`State` misuse caught explicitly. Releases are explicit (no `Drop` hook). Anchored values participate in GC as a new entry in `mark_gc_roots`.

### Public API

```rust
/// Opaque, Copy, Send. Bound to the State that produced it.
/// Use against a different State, or after release, returns
/// Err from operations that need a live value; never UB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    state_id: NonZeroU32,
    // Packed (slot: u32, generation: u32) via slotmap's KeyData::as_ffi.
    key: NonZeroU64,
}

impl State {
    /// Pop the top of stack and anchor it. Errors if the stack is
    /// empty or the top is `nil` (`nil` carries no GC weight and
    /// has no use case for a stable handle).
    pub fn anchor(&mut self) -> Result<Anchor>;

    /// Anchor a copy of stack[idx]. Does not pop. Errors on `nil`.
    pub fn anchor_at(&mut self, idx: isize) -> Result<Anchor>;

    /// Like `anchor` / `anchor_at` but additionally check the value
    /// is a function (Lua closure or `RustFunc`). Strict: tables with
    /// `__call` are rejected here. To anchor a callable table, use
    /// `anchor` and let the existing dispatch handle `__call` at call
    /// time. Convenience for embedders who want the type check at
    /// registration time rather than at first dispatch.
    pub fn anchor_function(&mut self) -> Result<Anchor>;
    pub fn anchor_function_at(&mut self, idx: isize) -> Result<Anchor>;

    /// Push the anchored value onto the stack. Errors with
    /// `ErrorKind::InvalidAnchor` if the handle is stale, released,
    /// or belongs to a different State.
    pub fn push_anchor(&mut self, a: Anchor) -> Result<()>;

    /// Convenience over `push_anchor` + `call`. Cost charges through
    /// the existing dispatch path; no new accounting.
    pub fn call_anchor(
        &mut self, a: Anchor,
        args: ArgCount, rets: RetCount,
    ) -> Result<()>;

    /// Release the slot. Returns `true` if the anchor was live;
    /// `false` for stale, already-released, or wrong-State handles.
    /// Idempotent: never errors, never panics.
    pub fn release_anchor(&mut self, a: Anchor) -> bool;

    /// Inspect the anchored value's type. None if the handle is not
    /// live in this State.
    pub fn anchor_type(&self, a: Anchor) -> Option<LuaType>;

    /// Number of live anchors. For embedder leak diagnostics.
    pub fn anchor_count(&self) -> usize;
}
```

`Anchor: Copy + Send + Sync + 'static`, 12 bytes. `Option<Anchor>` is also 12 bytes via the `NonZeroU32` / `NonZeroU64` niches.

### Storage

```rust
slotmap::new_key_type! { pub(crate) struct AnchorKey; }

pub(crate) struct Registry {
    state_id: NonZeroU32,
    slots: SlotMap<AnchorKey, Val>,
}
```

`SlotMap` is already the project's idiom for generational arenas (`src/vm/object.rs:142-149` for `ObjectKey`, `src/vm/object.rs:447-460` for `StringKey`). It's already a `Cargo.toml` dependency. Reusing it gets us:

- **Stale-handle detection for free.** Generations distinguish "the slot you anchored 100 calls ago" from "the slot we just recycled." luaL_ref reuses freed slots without generations (`research/lua-5.4/lauxlib.c:664-701`, `research/luau/VM/src/lapi.cpp:1546-1582`); ref-after-release silently lands on whatever was anchored next. Don't inherit that footgun.
- **Deterministic iteration** by slot index, matching `IndexMap`-for-globals discipline.
- **Slot reuse** so embedders that churn anchors don't accumulate dead slots forever.

`Anchor.key` packs the slotmap key via `KeyData::as_ffi() -> u64` (slotmap's documented stable encoding). NonZeroU64 because `KeyData` reserves the all-zero bit pattern.

### GC integration

Extend `mark_gc_roots` (`src/vm.rs:51-69`) with a `registry: &Registry` parameter, and have it iterate `registry.slots.values()` marking each `Val`. Update the single call site at `src/vm.rs:370-383`. CLAUDE.md is explicit: `mark_gc_roots` is the single source of truth for reachability; don't introduce a side root path.

Iteration order is slot-index order (slotmap's documented behavior), deterministic across hosts given identical anchor/release sequences. Same property `IndexMap<String, Val>` already gives for globals.

### Cross-`State` safety

Each `State` carries `state_id: NonZeroU32`, allocated from a process-wide `AtomicU32` at `Engine::new_state` time. Every `Anchor` carries the same `state_id`. Operations check it on entry and return `ErrorKind::InvalidAnchor` on mismatch. No UB, no silent corruption.

Why it matters: without this check, passing State A's anchor to State B would hit B's slotmap at the same slot with a probably-matching generation. Generations rule out *most* misuse but not all, and silent semantic-corruption is the worst class of bug to ship pre-1.0.

The 32-bit width is enough — `AtomicU32` wraps after 4B State allocations, by which point any pre-existing anchor is long gone. Don't try to cram `state_id` into the `key` niche; the bits are better spent on generation.

### Anchoring nil

Reject. `anchor` / `anchor_at` return `Err(ErrorKind::AnchorNil)` if the value is `Val::Nil`. Anchoring nil conflates "no anchor installed" with "valid anchor pointing at nil"; embedders who want the former use `Option<Anchor>` (which is niche-optimized to the same 12 bytes). luaL_ref's `LUA_REFNIL = 0` sentinel is a C ergonomics hack we don't need to inherit.

### Cost model

- `anchor*`, `release_anchor`, `push_anchor`, `anchor_type`, `anchor_count`: zero cost (rare host-API surface; not bytecode).
- `call_anchor`: sugar over `push_anchor + call`. Cost charges through the existing `State::call` path (`src/vm/eval.rs:31-141`), which already dispatches to Lua closures, RustFns, and `__call` tables uniformly. No new categories.

### Determinism

Three things to guard:

- **Marking order**: slotmap iterates by slot, deterministic given identical history. Same as `ObjectPtr` / `StringPtr`.
- **Handle bytes**: `KeyData::as_ffi()` is documented stable. Same insert/release sequence -> same handle bits across hosts.
- **`state_id` value**: process-allocator-derived, NOT byte-for-byte stable across hosts. Don't expose it via any to-be-serialized path. `Anchor::Debug` should redact it; treat it like a memory address.

### Interaction with `with_restricted_env`

Anchored values are **not** globals. `with_restricted_env` (`src/vm.rs:454-487`) swaps `globals` + `builtins`; the registry is untouched. That's exactly the property the original problem statement asks for: a script using a restricted environment doesn't see the embedder's retained handles.

## What this asks of embedders

```rust
let on_tick: Anchor = {
    state.get_global("on_tick");
    state.anchor_function()?    // type-checked at registration
};

// ...later, possibly across many turns:
state.call_anchor(on_tick, ArgCount::Fixed(0), RetCount::Fixed(0))?;

// when done:
state.release_anchor(on_tick);
```

Compared to today's `state.set_global("__handler_42", fn)` + `state.get_global("__handler_42")`, this is one line shorter at retain, one line shorter at call, type-checked at registration, deterministic across States, and invisible to scripts.

Discipline: call `release_anchor` when done. Leaks are bounded to State lifetime; on `State` drop, all anchored values free. Same discipline as `luaL_unref`.

## What we reject

- **Drop-cleaning newtype that releases on `Drop`.** Releasing requires `&mut State`, which a handle stored in a struct field can't materialize. Workarounds (an `Arc<Mutex<Vec<AnchorKey>>>` back-channel drained on next anchor call) add a `Sync`-bound side channel and a per-`State` mutex purely for `Drop` ergonomics. Not worth it; embedders who want RAII can build `OwnedAnchor<'s> { a: Anchor, s: &'s mut State }` themselves.

- **Type-parameterized `Anchor<Function>` / `Anchor<Table>` / `Anchor<Val>`.** Lua values are runtime-typed, and a table with `__call` is callable through the same `State::call` dispatch. Forcing the embedder to type-tag at registration creates false security and breaks the `__call` path. Provide `anchor_function*` for early type-checking, but the resulting handle is the same opaque `Anchor`.

- **Monotonic `NonZeroU32`.** Skips slot reuse; loses staleness detection. Slotmap costs us nothing extra and gives us both.

- **`IndexMap<RegistryRef, Val>`.** No slot reuse, no generations. All downside, no upside vs slotmap.

- **luaL_ref's silent slot reuse.** A C-API artifact, not a virtue. Generations cost almost nothing on top of slotmap and catch a real footgun.

- **`LUA_REFNIL`-style nil sentinel.** Conflates "no anchor" with "anchor of nil." `Option<Anchor>` is niche-optimized to the same size; use it.

- **Per-`Engine` shared anchors.** `Val` carries `ObjectPtr`/`StringPtr` keys that are valid only inside one `GcHeap` (`src/vm/object.rs:140-157`, `src/vm/object.rs:447-460`). Sharing across States would require sharing the heap, which the `State: Send`-by-isolation design (`notes/state-send.md`) deliberately avoids. `Program` is the cross-State artifact; runtime `Val`s aren't.

- **A separate "anchored values" GC root path.** `mark_gc_roots` is the single source of truth (CLAUDE.md, explicit). Add a parameter; don't add a side door.

- **Iter / public iteration of anchors.** `anchor_count` covers the leak-diagnostics use case. Exposing the iteration order risks embedders accidentally relying on it for program-visible behavior.

- **Idempotent `release_anchor` returning `()`.** Returning `bool` (was-live) costs nothing and helps embedders detect double-release without panicking. Idempotence is preserved.

## Implementation plan

About 150-200 LOC, no bytecode/dispatch changes. Single PR.

1. **`src/vm/anchor.rs` (new, ~100 LoC).** `AnchorKey`, `Registry`, `Anchor`, `Markable for Registry` impl iterating `slots.values()`. State-id allocation (process-wide `AtomicU32`).

2. **`src/vm.rs`.** Add `registry: Registry` field to `State` (the `state_id` lives on `Registry` itself; no need to duplicate). Initialize in `empty_with_callbacks`. Thread `&self.registry` into `mark_gc_roots` and the call site at `gc_collect`. Add the eight public methods (each 5-15 lines). `anchor_function*` is strict (`LuaType::Function` only): callable-table support would mean allocating `"__call"` and walking the metatable at registration time, which adds GC-interaction surface for marginal ergonomic gain. Embedders with callable tables use plain `anchor`.

3. **`src/error.rs`.** New `ErrorKind::InvalidAnchor` (covers stale, released, wrong-state). New `ErrorKind::AnchorNil` (anchor-of-nil rejected).

4. **`src/lib.rs`.** Re-export `Anchor`. `Engine::new_state*` allocates the next state_id.

5. **Tests** (new file `tests/anchor.rs`):
   - Anchor-survives-GC: anchor a closure, drop the global pointing at it, run GC, push and call the anchor, verify it works.
   - Release-collects: anchor + release + GC, verify the closure is collected.
   - Stale-after-release: anchor, release, attempt push -> `InvalidAnchor`.
   - Generation rejection: churn anchors, save an old handle, release-then-anchor in the same slot, verify old handle returns `InvalidAnchor`.
   - Cross-State rejection: two States, anchor in A, attempt push in B -> `InvalidAnchor`.
   - `with_restricted_env` invisibility: anchor a value, run a restricted-env block, verify the anchor still pushes the same value.
   - `anchor_function` rejects non-callables, accepts tables with `__call`.
   - `release_anchor` returns `false` on stale / wrong-state.
   - `nil` rejection: `anchor` / `anchor_at` on nil return `Err`.
   - Determinism: run the same anchor/release/call sequence on two `Engine::new_state()` instances; output identical.

Bench surface: nothing on the hot path. `mark_gc_roots` gets one extra `slotmap.values().for_each(mark)` per GC cycle; for the saehrimnir-scale "few dozen handlers" case this is unmeasurable.

## Open / deferred

- **`Anchor::EMPTY` sentinel for fixed-size handler tables.** `[Anchor; 64]` style. Skipped: `Option<Anchor>` is niche-optimized to the same size, and explicit `Option` is more idiomatic Rust. If a real embedder argues for it, revisit.
- **Engine-level handle for `Program`-related objects.** `Program` is already `Arc<Bytecode>` and `Send + Sync`; not a registry concern.
- **Deterministic `state_id` allocation.** Currently process-wide `AtomicU32`, fine for in-process use. If we ever ship a serialization story for `State` snapshots, `state_id` would need to be deterministic (e.g. a seed-derived value from `Engine`), but that's a 1.0-or-later concern.
- **Stable serialization of `Anchor` bits** — currently in-process only (state_id is non-deterministic across hosts). Same family of concern as the `RustFunc`-by-pointer-address note in `TODO.md`.
