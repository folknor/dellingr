# Threading story: Engine + Program + State

## Problem

`State` (the VM instance) is `!Send`. Embedders on a multi-threaded async runtime (axum, tokio worker pool, anything with a thread-pool executor) can't share a `State` across handler tasks. Workarounds today:

- Pin the VM to a dedicated `std::thread`, route dispatch through mpsc + oneshot. ~200 extra lines and an OS thread per VM.
- Switch the embedding runtime to current-thread / single-worker. Doesn't help when the framework needs `Send + Sync` on app state (axum, tonic).
- Fork dellingr.

## Why "just swap `Rc<Chunk>` -> `Arc<Chunk>`" doesn't work

The original framing assumed `Chunk` was immutable bytecode and the only fix was the refcount type. It isn't.

`Chunk` carries per-execution lookup caches:

- `global_lookup_cache: Vec<GlobalLookupCacheSlot>` (`src/compiler.rs:165`) where each slot is `Cell<Option<GlobalLookupCacheEntry>>`; entries store `globals_version: u64` keyed to one specific `State`.
- `field_lookup_cache: Vec<FieldLookupCacheSlot>` (`src/compiler.rs:166`) where the cached entry holds an `ObjectPtr` and `table_version` (`src/compiler.rs:58-63`). `ObjectPtr` is a `slotmap::Key` (`src/vm/object.rs:149`), valid only inside one `GcHeap`.
- `set_field_lookup_cache` (`src/compiler.rs:167`), `MethodLookupCacheEntry` and `StringMethodCacheEntry` (`src/compiler.rs:65-90`): same story.

Two consequences:

1. **`Chunk: !Sync`** because `Cell<T>: !Sync`. `Arc<T>: Send` requires `T: Send + Sync`. So `Arc<Chunk>: !Send`, so `State: !Send`. The minimal swap doesn't even compile.

2. Even patching with `AtomicCell` or `unsafe impl Sync` would be silently unsound across States that share an `Arc<Chunk>`. State A writes `ObjectPtr X / version 7` into a slot keyed to A's heap; State B then reads that slot and either panics with the slotmap's use-after-free check (`src/vm/object.rs:198-202`) or reads from a real-but-completely-unrelated table whose key happens to be in slot X with version 7. The author already noticed: `// Runtime lookup caches are State-specific, so cloned chunks start cold` (`src/compiler.rs:47`). `Arc::clone` doesn't clone, it shares.

So `Chunk` today is bytecode-plus-runtime-state. Threading needs that decoupled before anything else.

## Recommended design

Split the artifact, expose an `Engine` / `Program` / `State` triple matching the Rhai shape (and roughly the Boa shape, minus realms):

```rust
// Send + Sync. Holds compile-time config and the stdlib registry.
// One per app, shared via Arc across workers (or just &Engine).
pub struct Engine { /* ... */ }

// Send + Sync + Clone (Arc-backed). Frozen, cache-free compiled artifact.
// Loadable into any State produced by the same Engine.
pub struct Program(Arc<Bytecode>);

// Send, deliberately !Sync. Owns heap, stack, globals, budget,
// per-State runtime caches.
pub struct State { /* ... */ }
```

### Public API

```rust
impl Engine {
    pub fn new() -> Self;                   // with stdlib
    pub fn raw() -> Self;                   // empty, like Lua's lua_newstate
    pub fn compile(&self, src: &str) -> Result<Program>;
    pub fn compile_named(&self, src: &str, name: &str) -> Result<Program>;
    pub fn analyze_cost(&self, program: &Program) -> ScopeCost;

    pub fn new_state(&self) -> State;
    pub fn new_state_with_callbacks(&self,
        callbacks: Box<dyn HostCallbacks + Send>) -> State;
}

impl State {
    pub fn load(&mut self, program: &Program) -> Result<()>;
    pub fn load_string(&mut self, src: &str) -> Result<()>;  // sugar
    pub fn call(&mut self, args: ArgCount, rets: RetCount) -> Result<()>;
    pub fn set_user_data<T: Send + 'static>(&mut self, data: T);
    // existing set_cost_budget, set_rng_seed, etc. unchanged
}

pub trait HostCallbacks: Send {
    fn on_print(&mut self, source: Option<&str>, line: u32, message: &str) { ... }
    fn on_error(&mut self, source: Option<&str>, error: &Error) {}
}

// Compile-time witness so the property doesn't silently regress.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<State>();
    // deliberately do NOT assert Sync.
};
```

`RustFunc = fn(&mut State) -> Result<u8>` (`src/vm/lua_val.rs:14`) stays a function pointer. fn pointers are already `Send + Sync`. Don't regress to `Box<dyn Fn>` for parity with Rhai.

### Internal restructure

Split today's `Chunk` into immutable `Bytecode` (in `Program`) plus per-`State` `RuntimeCaches`:

```rust
// Send + Sync. Arc-shared via Program.
pub(crate) struct Bytecode {
    code: Vec<Instr>,
    number_literals: Vec<f64>,
    string_literals: Vec<Vec<u8>>,
    nested: Vec<Arc<Bytecode>>,
    upvalues: Vec<UpvalueDesc>,
    line_info: Vec<u32>,
    name: Option<String>, source: Option<String>,
    num_params: u8, num_locals: u8, is_vararg: bool,
    // Sizes only - each State allocates its own caches.
    global_cache_slots: u16,
    field_cache_slots: u16,
    set_field_cache_slots: u8,
}

// Per-Closure, per-State.
struct RuntimeCaches {
    global_lookup: Vec<Cell<Option<GlobalLookupCacheEntry>>>,
    field_lookup: Vec<FieldLookupCacheSlot>,
    set_field_lookup: Vec<SetFieldLookupCacheSlot>,
}

// SAFETY: RuntimeCaches contains Cells (!Sync in isolation). We claim Sync
// because every access goes through `&mut State`, and `State: !Sync`. The
// Cells are single-threaded at runtime; the unsafe impl just acknowledges
// that the type system can't see that invariant. Required for
// `Arc<RuntimeCaches>: Send` (which requires `T: Send + Sync`).
unsafe impl Sync for RuntimeCaches {}

pub(super) struct Closure {
    pub(super) bytecode: Arc<Bytecode>,        // was Rc<Chunk>
    pub(super) caches:  Arc<RuntimeCaches>,    // new
    pub(super) upvalues: Vec<UpvalueRef>,
}
```

Cache lookup sites at `frame.rs:665, 973, 1231` change from `frame.chunk.field_lookup_cache.get(idx)` to `frame.caches.field_lookup.get(idx)`. Same shape: one indexed `Vec` read. The compiler keeps emitting cache-slot indices into instructions exactly as it does today (`src/compiler.rs:184-238`); cache sizes propagate to `Bytecode`, and `RuntimeCaches::new(bytecode)` allocates the `Vec`s when a `Closure` is constructed.

What stays per-`State`: `GcHeap`, string interning, globals (`IndexMap`), upvalue pool, stack, `cost_remaining` / `cost_used`, `rng`, `current_source`. None of these are shared.

What becomes per-`Engine`: stdlib parsing (today `with_callbacks -> open_libs` re-runs the stdlib install per `State`, `src/vm.rs:176-180`; that becomes `Engine::new` once). Cost weights when they become configurable (per `TODO.md`).

## Implementation plan

One PR, no staging. Pre-1.0; the breakage budget covers it; doing the cache decoupling without exposing the `Engine` factory leaves a half-finished surface.

Approximate ordering, biggest items first:

1. **Split `Chunk` into `Bytecode` + `RuntimeCaches`.** Touches `compiler.rs` (cache types and `initialize_runtime_caches`), `vm/object.rs:97-100` (`Closure`), `vm/frame.rs:24,42,60-61,665,973,1231` (cache lookup sites), `vm/eval.rs:240-246,360,427` (call frame setup). 200-400 lines moved. `compiler::runtime_cache_tests` (`src/compiler.rs:258-333`) moves with the cache types.

2. **`Rc` -> `Arc` on the now-immutable `Bytecode`.** Mechanical.

3. **`Send` bounds.** `pub trait HostCallbacks: Send`, `Box<dyn HostCallbacks + Send>`, `Box<dyn Any + Send>`, `T: Send` on `set_user_data`. Update `replace_callbacks` return type. Update the doc example at `src/vm.rs:269` from `Rc<RefCell<CommandCollector>>` to either `Arc<Mutex<...>>` or just `MyContext { ... }` (the slot takes ownership; for `Send` types you usually don't need a wrapper).

4. **Engine + Program API.** Pure addition on top of steps 1-3. Move `parse_str` and `analyze_cost` (`src/lib.rs:295`) onto `Engine`. `State::from_engine`. `load_string` becomes sugar over `engine.compile(s)?` then `state.load(&program)`.

5. **Static `assert_send::<State>()`** in `src/lib.rs`.

Bench surface to watch: `examples/hotpath.rs` for `fields/same_obj_read`, `tables/fill`, `iter/pairs`, `alloc/closure`. The cache sites move from `frame.chunk.<cache>` to `frame.caches.<cache>` (same shape, possibly slightly better locality since `RuntimeCaches` is closer to `Frame` than the cache vectors are to `Chunk` today). The likely regression is on closure creation: `push_closure` (`src/vm/eval.rs:373`) now allocates three `Vec`s sized from `Bytecode`. If `examples/alloc/closure.lua` regresses materially, fall back to pooling the cache `Vec`s in `State` (the `UpvaluePool` template at `src/vm/object.rs:54-69` applies cleanly). Don't pool until measured.

Test surface: `tests/run_examples.rs`, `tests/error_handling.rs`, `tests/gc_upvalues.rs`. The diff_test harness is insensitive (output-only).

## What this asks of embedders

- **Already-`Send` `HostCallbacks` impls**: nothing.
- **`HostCallbacks` impls capturing `Rc<RefCell<...>>`**: switch to `Arc<Mutex<...>>`. The crate's own doc example (`src/vm.rs:268-276`) is the only known example and gets rewritten.
- **`set_user_data<T>` callers**: `T: Send`. The common case (game-state struct, command collector, channel handle) is already `Send`. The few legitimate non-`Send` types break at compile time.
- **Existing single-`State`, no-`Engine` users**: `State::new()` can stay as a deprecated convenience that creates an internal `Engine`, smoothing migration over one or two releases. Or it goes; pre-1.0.
- **Cost-budget semantics**: unchanged. One `State` = one budget = one tick.

## What we reject

- **`State: Sync` via internal locking.** `consume_cost` (`src/vm.rs:247`) is currently a single non-atomic dec. Under `Sync` it becomes either an atomic CAS per opcode (measurable on a VM with per-op cost charging) or a coarse-grained lock that can deadlock under callbacks (callback runs `&mut State`, can recurse into the VM). The cost-budget invariant *the action that pushes you over budget always completes* (`src/vm.rs:111-114`) only has a clean meaning under exclusive access. Embedders that think they want `Sync` want `Mutex<State>` at the embedder boundary, where the lock granularity is a request, not an opcode. They can write that themselves; the crate doesn't have to do it for them.

- **Feature-gated `sync` a la Rhai.** Rhai needs it because it has `Box<dyn Fn>` callbacks and `Shared<T> = Rc<T> | Arc<T>` aliasing across the codebase. dellingr has neither: `RustFunc` is a fn pointer, the only `Rc` is `Rc<Chunk>`, and the atomic-refcount delta is in the noise. Feature-flagging splits the test matrix and downstream crate feature graphs for vanishing benefit.

- **A parallel `SyncState` / `AsyncState` type.** Doubles the surface for one capability bit.

- **`Shared<T>` aliasing across the codebase.** Five sites. Just write `Arc`.

- **Boa-style `Realm`s.** Boa needs realms because ECMAScript has cross-realm semantics. Lua doesn't, and `restrict_globals` (`src/vm.rs:455-479`) already covers per-call sandboxing.

- **Pinning a VM to a dedicated `std::thread` with mpsc.** That's the workaround we're replacing.

- **Generic `State<U>` for typed user-data.** Tempting, but it infects `RustFunc = fn(&mut State)` and every signature that holds `&mut State`. The win over `Box<dyn Any + Send>` is one downcast per access. Defer indefinitely. If it ever lands, it lands at a 1.0 boundary as a separate change.

## Determinism

Nothing in this plan touches the determinism contract. `State` execution stays single-threaded by `!Sync`. Cost accounting stays on owned (non-atomic) integers. `IndexMap` iteration stays deterministic. The `StdRng` seeded via `set_rng_seed` (`src/vm.rs:217-219`) stays per-State. Two `State`s executing concurrently against shared `Arc<Engine>` and `Arc<Bytecode>` each remain individually deterministic; cross-State interleaving is observable only via the host, and the host owns that determinism story.

## Related findings (not part of this work, flagged here)

- `std::collections::hash_map::DefaultHasher` at `src/vm/object.rs:476` is documented as "internal algorithm not specified, subject to change." It's currently SipHash with a fixed key, so deterministic across hosts today, but a future stdlib release could silently change bucket order in `StringPool.hash_index`. The bucket order isn't program-visible (string interning compares by content, `src/vm/object.rs:494-503`), so the determinism contract holds, but this is a tripwire if the hasher is ever reused for something program-visible. Worth pinning to an explicit hasher (siphasher, ahash with a fixed seed, fnv) before that becomes load-bearing.

- `RustFunc` `Val` rendering hashes by function-pointer address (`src/vm/lua_val.rs:105, 169`). That's a pre-existing concern for byte-for-byte cross-host serialization, separate from the threading work, but worth a future thread.

- Once `Engine` exists, the stdlib doesn't need to be re-parsed per-`State`. `with_callbacks -> open_libs` (`src/vm.rs:176-180`) currently does. Move stdlib parsing to `Engine::new` once and have `new_state` reuse the result. Visible at "one State per request worker times N workers."
