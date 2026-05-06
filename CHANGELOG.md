# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Added

- `Anchor` retainable-value handle plus a per-`State` registry. Embedders
  can hold Lua values across host calls without polluting globals or
  using `with_restricted_env`-incompatible workarounds.
  - `State::anchor` / `State::anchor_at` - capture a `Val` from the stack.
  - `State::anchor_function` / `State::anchor_function_at` - same, with
    `LuaType::Function` enforcement at registration time.
  - `State::push_anchor` / `State::call_anchor` - push or push-and-call.
  - `State::release_anchor` - returns `bool` (was-live), idempotent on
    stale or wrong-`State` handles.
  - `State::anchor_type` / `State::anchor_count` - inspection helpers.
  - `Anchor` is `Copy + Send + Sync + 'static` (12 bytes); cross-`State`
    misuse returns `ErrorKind::InvalidAnchor` rather than aliasing into
    the wrong heap.
  - Backed by `slotmap::SlotMap` so generations catch use-after-release.
- `Engine` factory type (`Send + Sync`) for compiling Lua source into
  reusable `Program` handles and creating `State` instances.
  `Engine::compile`, `Engine::compile_named`, `Engine::analyze_cost`,
  `Engine::new_state`, `Engine::new_state_with_callbacks`. Compile once
  on the engine, load into many states.
- `Program(Arc<Bytecode>)` - clonable, `Send + Sync`, shareable across
  states. Carries the immutable compiled bytecode without per-state
  runtime caches.
- `State::load(&Program)` - load a compiled program onto a state's
  stack as a callable closure. Pairs with the existing
  `State::call(...)`.
- Compile-time `assert_send::<State>()` witness in the crate root so
  the property cannot silently regress.

### Changed (breaking)

- `State` is now `Send`. Embedders can move a `State` across threads
  (e.g. into a tokio task, or behind `Mutex<State>` shared via `Arc`
  across worker threads). `State` is intentionally NOT `Sync` -
  cost-budgeted dispatch only has well-defined semantics under
  exclusive access; use `Mutex<State>` at the embedder boundary.
- `HostCallbacks: Send`. Implementors that previously held `Rc` or
  `RefCell` must switch to `Arc` / `Mutex` (or drop them - most
  callbacks don't need shared interior state).
- `State::with_callbacks` / `State::replace_callbacks` now take
  `Box<dyn HostCallbacks + Send>`.
- `State::set_user_data<T: Send + 'static>` (and `user_data` /
  `user_data_mut`) - the user-data slot is now
  `Box<dyn Any + Send>`. The doc example switches from
  `Rc<RefCell<...>>` to `Arc<Mutex<...>>`.

### Internal

- Split the previous `Chunk` type into immutable `Bytecode`
  (instructions, literal pools, cache-slot counts) and per-`Closure`
  `RuntimeCaches` (the `Cell`-backed lookup vectors). `Closure` and
  `Frame` now hold `Arc<Bytecode>` plus `Arc<RuntimeCaches>`. The
  cache topology is unchanged on the dispatch hot path - one indexed
  `Vec` read per cache slot - and the cache contents are still shared
  across recursive frames on the same closure.

### Performance

- String-heavy workloads run roughly 2x faster across the board. The
  string pool's interner now indexes by hash for O(1) lookup instead
  of a linear scan, chained `..` collapses into a single OP_CONCAT(n)
  instead of n-1 binary concats, and the concat buffer is pre-sized.
  On the `strings/mixed` bench the wall time dropped 99ms -> 43ms; on
  `strings/patterns` 38ms -> 30ms.
- Field reads on a stable receiver (`entity.x` accessed in a hot loop)
  now go through a per-callsite inline cache - the existing field IC
  was already doing this, but the cache machinery has been extended
  to cover `obj:method()` dispatch through metatables and `s:method()`
  dispatch through the `string` library.
- Field writes have a matching SET_FIELD inline cache; the slow path
  now also populates it on first insert.
- Table mutation no longer invalidates the field cache for unrelated
  inserts on the same table (only removes / shifts bump the table
  version, since insertions don't move existing entries' indices).
- Numeric integer indexing is faster on hash-storage tables. For
  positive finite integer keys, `Table::get` now tries `IndexMap`
  position `n-1` directly, validates by bit-equality of the stored
  key, and only falls back to the hash lookup on miss. On the
  `tables/numeric_index` bench wall time dropped 112ms -> 73ms;
  `tables/fill` 105ms -> 98ms; `tables/mixed` 102ms -> 89ms;
  `fields/same_obj_read` is unchanged.

## [0.1.0] - 2026-05-05

Initial release.
