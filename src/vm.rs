//! This module provides the `State` struct, which handles the primary
//! components of the VM.

mod anchor;
mod eval;
mod eval_control;
mod eval_index;
mod eval_store;
mod frame;
mod global_env;
mod lua_val;
mod metamethod;
mod object;
mod rng;
#[cfg(feature = "snapshot")]
mod save_state;
mod stack;
mod table;
mod table_ops;

pub use anchor::Anchor;
pub use lua_val::LuaType;
pub use lua_val::RustFunc;
#[cfg(feature = "snapshot")]
pub use save_state::{LoadError, SaveDiagnostics, SaveError, SaveState};

use crate::det_hash::IndexMap;
#[cfg(feature = "snapshot")]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use super::Instr;
use super::Result;
use super::compiler;
use super::compiler::Bytecode;
use super::compiler::RuntimeCaches;
use super::cost_meter::{CostMeter, CostState};
use super::error::Error;
use super::error::ErrorKind;
use super::error::StackFrame;
use super::error::TypeError;
use super::host::{DefaultCallbacks, HostCallbacks};
use super::instr::{ArgCount, Builtin, RetCount};
use super::patterns::LuaPattern;

use anchor::Registry;
use global_env::GlobalEnv;

pub(super) use lua_val::Val;
pub(super) use object::ObjectPtr;
use object::{GcHeap, GcPolicy, Markable, StringPtr, UpvaluePool, UpvalueRef};
use rng::VmRng;
use table::Table;

pub use crate::limits::MAX_STRING_BYTES;
use crate::limits::{
    GMATCH_PATTERN_CACHE_BYTES, GMATCH_PATTERN_CACHE_ENTRIES, MAX_CALL_DEPTH, MAX_STACK_SIZE,
};
// Capacity hints for `reserve_stdlib_capacity`; they size allocations only and
// never affect behaviour or iteration order. Keep them in step with the
// registrations in `lua_std`. Every builtin slot is also a global, and `error`
// is the one standard-library global without a builtin slot.
const STDLIB_NON_BUILTIN_GLOBALS: usize = 1;
// Keep room for a few host-defined globals alongside the standard library.
const HOST_GLOBAL_CAPACITY_HEADROOM: usize = 4;
// `_G`, its metatable, the math, string and table library tables, and the
// string metatable.
const STDLIB_HEAP_TABLES: usize = 6;
// Distinct interned keys across those tables: math's functions plus `pi` and
// `huge`, the string and table functions, and `_G`'s `__index`/`__newindex`.
const STDLIB_INTERNED_TABLE_KEYS: usize = 41;
/// Snapshot environment token of the shared string metatable. Like
/// `_G.metatable`, it lives in the token namespace, not the global one.
#[cfg(feature = "snapshot")]
const STRING_METATABLE_TOKEN: &str = "string.metatable";

struct GmatchPatternEntry {
    pattern: LuaPattern,
    byte_len: usize,
}

pub(crate) fn check_string_size(size: usize) -> Result<()> {
    if size > MAX_STRING_BYTES {
        return Err(Error::without_location(ErrorKind::StringSizeExceeded {
            size,
            limit: MAX_STRING_BYTES,
        }));
    }
    Ok(())
}

pub(crate) fn checked_string_growth(current: usize, additional: usize) -> Result<usize> {
    // Saturating rather than wrapping: an overflowing total is reported as
    // usize::MAX, which is above the cap and so rejected, instead of wrapping
    // to a small value that would pass the check.
    let size = current.saturating_add(additional);
    check_string_size(size)?;
    Ok(size)
}

/// Values temporarily held outside the visible VM stack.
pub(super) struct TransientRoots {
    values: Vec<Val>,
    suspended_envs: Vec<GlobalEnv>,
}

impl TransientRoots {
    #[cfg(feature = "snapshot")]
    fn is_empty(&self) -> bool {
        self.values.is_empty() && self.suspended_envs.is_empty()
    }
}

impl Markable for TransientRoots {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        self.values.mark_reachable(heap, worklist);
        self.suspended_envs.mark_reachable(heap, worklist);
    }
}

/// Marks all GC roots. Called before garbage collection to identify reachable objects.
///
/// This function is the single source of truth for what constitutes a GC root.
/// All allocation functions that may trigger GC must call this with the same set of roots.
///
#[hotpath::measure]
pub(super) fn mark_gc_roots(state: &State, worklist: &mut Vec<ObjectPtr>) {
    // Mark all roots - closed upvalues are now marked transitively when
    // marking LuaFn closures that reference them
    state.stack.mark_reachable(&state.heap, worklist);
    state.global_env.mark_reachable(&state.heap, worklist);
    for (identity, entry) in &state.bytecode_caches {
        debug_assert_eq!(*identity, Arc::as_ptr(&entry.bytecode) as usize);
        entry.runtime.literals.mark_reachable(&state.heap, worklist);
    }
    state.transient_roots.mark_reachable(&state.heap, worklist);
    state.registry.mark_reachable(&state.heap, worklist);
    // Strings carry no metatable pointer, so the shared metatable must remain
    // rooted even when no Lua value explicitly references it. Use `GcHeap::mark`
    // so the object itself is coloured, not only its children.
    if let Some(mt) = state.string_metatable {
        state.heap.mark(mt, worklist);
    }
    #[cfg(feature = "snapshot")]
    {
        // The canonical environment and its pristine baseline must survive even
        // when user code currently reaches none of it: save/load diffs live
        // against this snapshot.
        // Must go through `GcHeap::mark`, which sets the object's color before
        // queueing it. Pushing onto the worklist directly leaves the object
        // Unmarked, so its children are traced but sweep still frees it - and a
        // later save then dereferences a dangling canonical environment table.
        for ptr in state.env_tokens.keys() {
            state.heap.mark(*ptr, worklist);
        }
        for baseline in state.env_baselines.values() {
            for (key, value) in &baseline.entries {
                key.mark_reachable(&state.heap, worklist);
                value.mark_reachable(&state.heap, worklist);
            }
            if let Some(metatable) = &baseline.metatable {
                metatable.mark_reachable(&state.heap, worklist);
            }
        }
    }
    // Note: open upvalues point to stack (already marked), closed upvalues
    // are marked transitively through the closures that reference them
}

/// Information about an active function call, used for stack traces.
#[derive(Clone)]
pub(super) struct CallInfo {
    /// The bytecode being executed.
    pub(super) bytecode: Arc<Bytecode>,
    /// Current instruction pointer.
    pub(super) ip: usize,
}

/// State-local runtime data for one immutable bytecode chunk.
#[derive(Debug)]
pub(super) struct BytecodeRuntime {
    pub(super) literals: Box<[Val]>,
    pub(super) caches: RuntimeCaches,
}

struct BytecodeCacheEntry {
    /// Pins the pointer-derived map identity against allocator ABA reuse.
    bytecode: Arc<Bytecode>,
    runtime: Arc<BytecodeRuntime>,
}

/// The main interface into the Lua VM.
pub struct State {
    /// Global bindings and their cache invalidation policy.
    global_env: GlobalEnv,
    /// The metatable shared by every string, as in reference Lua. Built by
    /// `open_libs` with `__index` = the string library table it created, so
    /// string indexing (`s:upper()`) never consults the global `string`.
    /// `None` in a `State::empty` that never opened the libraries. Not a
    /// global: `with_restricted_env` leaves it alone. A GC root.
    pub(super) string_metatable: Option<ObjectPtr>,
    /// `(shape version, slot)` of `__index` in `string_metatable`, recorded
    /// by the string-method IC's slow path so its fast path can re-check the
    /// `__index` value without a hash lookup. Reset when the metatable is
    /// replaced; a stale entry only causes misses.
    pub(super) string_mt_index_shape: Option<(u64, usize)>,
    /// The main stack which stores values.
    pub(super) stack: Vec<Val>,
    /// The bottom index of the current frame in the stack.
    pub(super) stack_bottom: usize,
    /// The heap which holds any garbage-collected Objects.
    pub(super) heap: GcHeap,
    /// State-local runtime data, keyed by the pinned Bytecode allocation.
    bytecode_caches: IndexMap<usize, BytecodeCacheEntry>,
    /// Identities being turned into closure shells, retained during a GC sweep.
    pending_bytecode_caches: Vec<usize>,
    /// Lua closure objects removed from the visible stack while they execute.
    pub(super) transient_roots: TransientRoots,
    /// Pool for upvalue storage. Avoids per-upvalue heap allocations.
    pub(super) upvalue_pool: UpvaluePool,
    /// Open upvalues currently pointing to stack slots.
    /// Each entry is (stack_index, upvalue_ref). Kept sorted by stack_index ascending
    /// so we can efficiently close them when a function returns.
    pub(super) open_upvalues: Vec<(usize, UpvalueRef)>,
    /// Dynamic call and constructor bases awaiting their consuming opcode.
    /// Each Lua frame owns the entries above the floor it recorded on entry.
    pub(in crate::vm) pending_bases: frame::PendingBases,
    /// Runtime cost accounting and optional budget.
    pub(super) cost: CostState,
    /// Compiled patterns retained for active `string.gmatch` iterators.
    ///
    /// Keys are interned string pointers, so cache lookup does not compare
    /// pattern bytes. Insertion order makes eviction deterministic.
    /// It is an implementation cache only: iterator state remains entirely in
    /// Lua values and a snapshot simply rebuilds entries on later iteration.
    gmatch_patterns: IndexMap<StringPtr, GmatchPatternEntry>,
    gmatch_pattern_bytes: usize,
    #[cfg(debug_assertions)]
    gmatch_pattern_compilations: u64,
    /// Current metamethod call depth (for __index/__newindex chains).
    /// Prevents stack overflow from circular metamethod references.
    pub(super) metamethod_depth: u32,
    /// Nesting of non-function `__call` handler resolution. Only the outermost
    /// level of such a chain reports trace-less errors to `on_error`; nested
    /// Rust callbacks defer to it. Kept separate from `metamethod_depth` so a
    /// Rust `__index`/`__newindex` handler failing under a host table access
    /// still reports at its own boundary.
    pub(super) call_chain_depth: u32,
    /// Active native callback, restored across nested Rust calls.
    pub(super) active_rust_fn: Option<RustFunc>,
    /// Current function call depth. Prevents stack overflow from deep recursion.
    pub(super) call_depth: u32,
    /// Call stack for generating stack traces on errors.
    /// Each entry represents an active Lua function call.
    pub(super) call_stack: Vec<CallInfo>,
    /// Host callbacks for print output, error handling, etc.
    pub(super) callbacks: Box<dyn HostCallbacks>,
    /// User-defined data that RustFuncs can access.
    /// Use `set_user_data<T>()` and `user_data<T>()` to store/retrieve.
    user_data: Option<Box<dyn std::any::Any + Send>>,
    /// Seeded RNG for deterministic math.random(). Defaults to seed 0.
    /// Use `set_rng_seed()` to set a specific seed for replay.
    pub(super) rng: VmRng,
    /// Registry of values retained from Rust via `Anchor` handles. Acts as
    /// an additional GC root set; participates in `mark_gc_roots`. Carries
    /// the State's process-unique `state_id` so cross-State misuse of an
    /// `Anchor` is caught.
    pub(super) registry: Registry,
    /// Lazily assigned deterministic identities used by `string.format("%p")`.
    /// These values deliberately are not GC roots; generational keys prevent a
    /// later allocation from aliasing an identity belonging to a freed value.
    // Insertion order is serialized. Do not prune outside gc_collect: changing
    // which entries remain must not change snapshot traversal order. With GC
    // disabled, entries grow with observed objects, just as the heap does.
    pub(super) format_pointer_ids: IndexMap<Val, u64>,
    pub(super) next_format_pointer_id: u64,
    /// Stable ids for Rust functions that are allowed to survive save/load.
    #[cfg(feature = "snapshot")]
    // Registrations persist even while unreachable: the host may push the same
    // function again, and snapshots must still resolve its stable name. An
    // unregister API needs an explicit host lifetime contract; automatic
    // pruning would change save success and reverse-name selection.
    pub(super) rust_fns_by_id: BTreeMap<String, RustFunc>,
    /// Reverse address lookup for save-time Rust function naming.
    #[cfg(feature = "snapshot")]
    pub(super) rust_fn_ids_by_addr: BTreeMap<usize, String>,
    /// Canonical environment objects (library tables, `_G`, `_G`'s metatable,
    /// and the shared string metatable) captured after each successful open_libs,
    /// each mapped to its save token. The public `open_libs` refreshes this map,
    /// and with_callbacks uses that same helper.
    /// Keyed on the objects `open_libs` built, so user shadowing of a builtin
    /// slot (`math = {}`) cannot make the save classifier mistake a user table
    /// for an environment object. See `vm/save_state.rs`.
    #[cfg(feature = "snapshot")]
    pub(super) env_tokens: BTreeMap<ObjectPtr, String>,
    /// Pristine ordered contents captured alongside `env_tokens`. These are
    /// compared at save time so mutations of rebuilt library tables persist.
    #[cfg(feature = "snapshot")]
    pub(super) env_baselines: BTreeMap<ObjectPtr, EnvBaseline>,
}

#[cfg(feature = "snapshot")]
pub(super) struct EnvBaseline {
    pub(super) entries: Vec<(Val, Val)>,
    pub(super) metatable: Option<Val>,
}

// Important note on how the stack is tracked:
// A State uses a single stack for all local variables, temporary values,
// function arguments, and function return values. Both Lua frames and Rust
// frames use this stack. `self.stack_bottom` refers to the first value in the
// stack which belongs to the current frame. Note that Rust functions access
// the stack using 1-based indexing, but Lua code uses 0-based indexing.

impl State {
    /// Creates a new, independent state with default callbacks (stdout).
    pub fn new() -> Self {
        Self::with_callbacks(Box::new(DefaultCallbacks))
    }

    /// Creates a new state with custom host callbacks.
    ///
    /// # Example
    ///
    /// ```ignore
    /// struct MyCallbacks { output: Vec<String> }
    /// impl HostCallbacks for MyCallbacks {
    ///     fn on_print(&mut self, _source: Option<&str>, _line: u32, message: &str) {
    ///         self.output.push(message.to_string());
    ///     }
    /// }
    ///
    /// let mut state = State::with_callbacks(Box::new(MyCallbacks { output: vec![] }));
    /// ```
    pub fn with_callbacks(callbacks: Box<dyn HostCallbacks>) -> Self {
        Self::with_callbacks_and_policy(callbacks, GcPolicy::default())
    }

    /// Creates a state with a host-selected garbage collection policy.
    ///
    /// The initial threshold is active while the standard library is opened.
    /// After an automatic collection, the next threshold is
    /// `max(live_allocations * growth_factor, minimum_threshold)`. These values
    /// are runtime policy and are not written to snapshots.
    ///
    /// `usize::MAX` as `initial_threshold` disables automatic collection until
    /// the threshold is changed. A zero threshold triggers collection before
    /// each allocation. The default policy is `(20, 20, 2)`.
    pub fn with_gc_policy(
        initial_threshold: usize,
        minimum_threshold: usize,
        growth_factor: usize,
    ) -> Self {
        Self::with_callbacks_and_policy(
            Box::new(DefaultCallbacks),
            GcPolicy::new(initial_threshold, minimum_threshold, growth_factor),
        )
    }

    /// Creates a state with custom callbacks and a host-selected GC policy.
    ///
    /// The initial threshold is active while the standard library is opened.
    /// After an automatic collection, the next threshold is
    /// `max(live_allocations * growth_factor, minimum_threshold)`. These values
    /// are runtime policy and are not written to snapshots.
    ///
    /// `usize::MAX` as `initial_threshold` disables automatic collection until
    /// the threshold is changed. A zero threshold triggers collection before
    /// each allocation. The default policy is `(20, 20, 2)`.
    pub fn with_callbacks_and_gc_policy(
        callbacks: Box<dyn HostCallbacks>,
        initial_threshold: usize,
        minimum_threshold: usize,
        growth_factor: usize,
    ) -> Self {
        Self::with_callbacks_and_policy(
            callbacks,
            GcPolicy::new(initial_threshold, minimum_threshold, growth_factor),
        )
    }

    fn with_callbacks_and_policy(callbacks: Box<dyn HostCallbacks>, policy: GcPolicy) -> Self {
        let mut me = Self::empty_with_callbacks_and_gc_policy(callbacks, policy);
        me.open_libs()
            .expect("standard library initialization starts with an empty value stack");
        me
    }

    /// Record the canonical environment objects right after `open_libs`, before
    /// any user code can shadow a builtin slot. Both save (object -> token) and
    /// load (token -> object) resolve through this snapshot.
    #[cfg(feature = "snapshot")]
    pub(crate) fn capture_env_tokens(&mut self) {
        let mut map = BTreeMap::new();
        for slot in [Builtin::Math, Builtin::String, Builtin::Table, Builtin::G] {
            if let Val::Obj(ptr) = self.global_env.builtin(slot as u8) {
                map.insert(ptr, slot.name().to_string());
                if slot == Builtin::G
                    && let Some(table) = self.heap.as_table_ref(ptr)
                    && let Some(mt) = table.get_metatable()
                {
                    map.insert(mt, "_G.metatable".to_string());
                }
            }
        }
        // The shared string metatable is rebuilt by `open_libs` too, so a
        // script holding it (`local mt = getmetatable("")`) reloads it as the
        // live object, and a change to its `__index` persists as a delta.
        if let Some(mt) = self.string_metatable {
            map.insert(mt, STRING_METATABLE_TOKEN.to_string());
        }
        let baselines = map
            .keys()
            .map(|ptr| {
                let table = self
                    .heap
                    .as_table_ref(*ptr)
                    .expect("canonical environment object is a table");
                (
                    *ptr,
                    EnvBaseline {
                        entries: table.entries(),
                        metatable: table.get_metatable().map(Val::Obj),
                    },
                )
            })
            .collect();
        self.env_tokens = map;
        self.env_baselines = baselines;
    }

    /// Creates a new state without opening any of the standard libs.
    /// The global namespace of this state is entirely empty. This corresponds
    /// to the `lua_newstate' function in the C API.
    pub fn empty() -> Self {
        Self::empty_with_callbacks(Box::new(DefaultCallbacks))
    }

    /// Creates an empty state with custom callbacks.
    pub(crate) fn empty_with_callbacks(callbacks: Box<dyn HostCallbacks>) -> Self {
        Self::empty_with_callbacks_and_gc_policy(callbacks, GcPolicy::default())
    }

    fn empty_with_callbacks_and_gc_policy(
        callbacks: Box<dyn HostCallbacks>,
        gc_policy: GcPolicy,
    ) -> Self {
        let state_id = anchor::next_state_id();
        Self {
            global_env: GlobalEnv::new(),
            string_metatable: None,
            string_mt_index_shape: None,
            stack: Vec::with_capacity(256), // Pre-size for typical function depth * locals
            stack_bottom: 0,
            heap: GcHeap::with_policy(gc_policy),
            bytecode_caches: IndexMap::default(),
            pending_bytecode_caches: Vec::new(),
            transient_roots: TransientRoots {
                values: Vec::with_capacity(64),
                suspended_envs: Vec::new(),
            },
            upvalue_pool: UpvaluePool::new(),
            open_upvalues: Vec::new(),
            pending_bases: frame::PendingBases::default(),
            cost: CostState::default(),
            gmatch_patterns: IndexMap::default(),
            gmatch_pattern_bytes: 0,
            #[cfg(debug_assertions)]
            gmatch_pattern_compilations: 0,
            metamethod_depth: 0,
            call_chain_depth: 0,
            active_rust_fn: None,
            call_depth: 0,
            call_stack: Vec::with_capacity(64), // Pre-size for call stack
            callbacks,
            user_data: None,
            rng: VmRng::seed_from_u64(VmRng::DEFAULT_SEED),
            registry: Registry::new(state_id),
            format_pointer_ids: IndexMap::default(),
            next_format_pointer_id: 1,
            #[cfg(feature = "snapshot")]
            rust_fns_by_id: BTreeMap::new(),
            #[cfg(feature = "snapshot")]
            rust_fn_ids_by_addr: BTreeMap::new(),
            #[cfg(feature = "snapshot")]
            env_tokens: BTreeMap::new(),
            #[cfg(feature = "snapshot")]
            env_baselines: BTreeMap::new(),
        }
    }

    pub(crate) fn reserve_stdlib_capacity(&mut self) {
        self.global_env
            .reserve(Builtin::COUNT + STDLIB_NON_BUILTIN_GLOBALS + HOST_GLOBAL_CAPACITY_HEADROOM);
        self.heap
            .reserve(STDLIB_HEAP_TABLES, STDLIB_INTERNED_TABLE_KEYS);
    }

    /// Sets the RNG seed for deterministic math.random() behavior.
    pub fn set_rng_seed(&mut self, seed: u64) {
        self.rng = VmRng::seed_from_u64(seed);
    }

    /// Sets the cost budget for this VM.
    /// When the budget is exhausted, operations with cost > 0 will fail.
    /// The action that pushes you over budget always completes before stopping.
    ///
    /// This configures a finite budget, including for `i64::MAX`; it does not
    /// return the VM to count-only mode. Negative budgets are accepted.
    ///
    /// Also resets [`State::cost_used`] to zero. Calling it from inside a Rust
    /// callback (mid-run) is supported: the Lua caller's batched opcode cost is
    /// always flushed before any call leaves the dispatch loop, so the reset
    /// sees a complete meter and `cost_used` afterwards counts only work done
    /// after the reset, independent of the batching interval.
    pub fn set_cost_budget(&mut self, budget: i64) {
        self.cost.set_budget(budget);
    }

    /// Returns the total cost consumed since the last budget reset.
    pub fn cost_used(&self) -> u64 {
        self.cost.used()
    }

    /// Returns the cost remaining in the budget.
    /// Can be negative if the last action pushed over budget.
    /// Without a configured budget, returns the unbounded sentinel `i64::MAX`.
    pub fn cost_remaining(&self) -> i64 {
        self.cost.remaining()
    }

    /// Test instrumentation for the gmatch compilation cache.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn gmatch_pattern_compilations(&self) -> u64 {
        self.gmatch_pattern_compilations
    }

    /// Consume cost from the budget. Returns an error if budget is exhausted
    /// and cost > 0. The action that pushes you over budget completes before
    /// stopping (checked at the START of each operation).
    ///
    /// Use this in RustFuncs to charge for expensive operations.
    #[inline(always)]
    pub fn consume_cost(&mut self, cost: u64) -> Result<()> {
        if self.cost_meter().consume(cost) {
            Ok(())
        } else {
            Err(self.budget_exceeded_error())
        }
    }

    /// Returns a meter for runtime work that does not need VM state.
    pub(crate) fn cost_meter(&mut self) -> CostMeter<'_> {
        self.cost.meter()
    }

    /// Flush before batching would hide exhaustion of a configured budget.
    pub(super) fn cost_batch_needs_flush(&self, pending: u64) -> bool {
        self.cost.batch_needs_flush(pending)
    }

    /// Compile and retain a `string.gmatch` pattern on its first iteration.
    ///
    /// The iterator charges compilation once on first use. Malformed pattern
    /// points are encoded in the compiled matcher and raise only when reached.
    pub(crate) fn memoize_gmatch_pattern(&mut self, idx: isize) -> Result<()> {
        // Preserve the caller's relative argument number even inside a nested
        // native frame. Wrong types in saved iterator state use the same
        // argument-type contract as to_bytes, not a cache-ordering error.
        let idx = self.expect_string_at(idx)?;
        let val = &self.stack[idx];
        let ptr = val
            .as_string_ptr()
            .expect("expect_string_at validated the value's string type");
        if self.gmatch_patterns.contains_key(&ptr) {
            return Ok(());
        }
        let pattern_len = val
            .as_string(&self.heap)
            .expect("string pointer came from a string value")
            .len();
        // Compilation is an implementation cache effect. The iterator charges
        // its first use once in saved Lua state, regardless of cache residency.
        #[cfg(debug_assertions)]
        {
            self.gmatch_pattern_compilations = self.gmatch_pattern_compilations.saturating_add(1);
        }
        let compiled = LuaPattern::new(
            self.stack[idx]
                .as_string(&self.heap)
                .expect("string pointer came from a string value"),
        );
        while self.gmatch_patterns.len() >= GMATCH_PATTERN_CACHE_ENTRIES
            || self.gmatch_pattern_bytes.saturating_add(pattern_len) > GMATCH_PATTERN_CACHE_BYTES
        {
            let (_, evicted) = self
                .gmatch_patterns
                .shift_remove_index(0)
                .expect("a nonempty gmatch cache has an oldest entry");
            self.gmatch_pattern_bytes = self.gmatch_pattern_bytes.saturating_sub(evicted.byte_len);
        }
        self.gmatch_pattern_bytes = self.gmatch_pattern_bytes.saturating_add(pattern_len);
        self.gmatch_patterns.insert(
            ptr,
            GmatchPatternEntry {
                pattern: compiled,
                byte_len: pattern_len,
            },
        );
        Ok(())
    }

    /// Returns a gmatch subject borrow, its cached matcher, and a cost meter.
    ///
    /// These values come from disjoint `State` fields. Keeping the split here
    /// lets the iterator match directly against the stack-resident subject.
    pub(crate) fn gmatch_subject_matcher_and_cost_meter(
        &mut self,
        subject_idx: isize,
        pattern_idx: isize,
    ) -> Result<(&[u8], &mut LuaPattern, CostMeter<'_>)> {
        // Type failures describe supplied iterator state, not cache ordering.
        // Validate before converting frame-relative indexes to absolute slots.
        let subject_idx = self.expect_string_at(subject_idx)?;
        let pattern_idx = self.expect_string_at(pattern_idx)?;
        let State {
            stack,
            heap,
            cost,
            gmatch_patterns,
            ..
        } = self;
        let subject = stack[subject_idx]
            .as_string(heap)
            .expect("expect_string_at validated the subject's string type");
        let pattern_ptr = stack[pattern_idx]
            .as_string_ptr()
            .expect("expect_string_at validated the pattern's string type");
        // The sole caller is gmatch_iter: it memoizes immediately before this
        // borrow, with no allocation or collection between the two calls.
        // A missing entry is a caller-ordering failure rather than a script-
        // reachable state. Report it instead of asserting, so a future caller
        // that gets the order wrong sees an error instead of a host panic.
        let entry = gmatch_patterns.get_mut(&pattern_ptr).ok_or_else(|| {
            Error::without_location(ErrorKind::InternalError(
                "gmatch pattern was not memoized before matching".into(),
            ))
        })?;
        let matcher = &mut entry.pattern;
        let meter = cost.meter();
        Ok((subject, matcher, meter))
    }

    pub(crate) fn budget_exceeded_error(&self) -> Error {
        self.error(ErrorKind::BudgetExceeded {
            used: self.cost.used(),
            budget: self.cost.limit(),
        })
    }

    // ========================================================================
    // User data
    // ========================================================================

    /// Store arbitrary user data that RustFuncs can access.
    ///
    /// Useful for passing context to Rust callbacks, like a command collector.
    /// `T` must be `Send` because `State` is `Send`: any data the embedder
    /// hands to the VM must be safe to move across threads with the State.
    ///
    /// # Example
    /// ```ignore
    /// let collector = Arc::new(Mutex::new(CommandCollector::default()));
    /// state.set_user_data(collector.clone());
    ///
    /// state.push_rust_fn(|state| {
    ///     let collector = state.user_data::<Arc<Mutex<CommandCollector>>>().unwrap();
    ///     collector.lock().unwrap().turn = Some(0.5);
    ///     Ok(0)
    /// })?;
    /// ```
    pub fn set_user_data<T: Send + 'static>(&mut self, data: T) {
        self.user_data = Some(Box::new(data));
    }

    /// Get a reference to the stored user data.
    /// Returns None if no data is stored or if the type doesn't match.
    pub fn user_data<T: Send + 'static>(&self) -> Option<&T> {
        self.user_data.as_ref()?.downcast_ref()
    }

    /// Get a mutable reference to the stored user data.
    /// Returns None if no data is stored or if the type doesn't match.
    pub fn user_data_mut<T: Send + 'static>(&mut self) -> Option<&mut T> {
        self.user_data.as_mut()?.downcast_mut()
    }

    /// Clear the stored user data.
    pub fn clear_user_data(&mut self) {
        self.user_data = None;
    }

    // ========================================================================
    // Callbacks
    // ========================================================================

    /// Get a mutable reference to the host callbacks.
    ///
    /// Use this to retrieve collected print output or other callback state.
    pub fn callbacks_mut(&mut self) -> &mut dyn HostCallbacks {
        self.callbacks.as_mut()
    }

    /// Replace the host callbacks with new ones, returning the old callbacks.
    pub fn replace_callbacks(
        &mut self,
        callbacks: Box<dyn HostCallbacks>,
    ) -> Box<dyn HostCallbacks> {
        std::mem::replace(&mut self.callbacks, callbacks)
    }

    // ========================================================================
    // Memory tracking
    // ========================================================================

    /// Returns the number of GC-managed objects (tables and closures).
    pub fn object_count(&self) -> usize {
        self.heap.object_count()
    }

    /// Returns the number of interned strings.
    pub fn string_count(&self) -> usize {
        self.heap.string_count()
    }

    /// Returns the total number of heap allocations (objects + strings).
    pub fn heap_size(&self) -> usize {
        self.heap.allocation_count()
    }

    // ========================================================================
    // Host-controlled GC
    // ========================================================================

    /// Returns true if `heap_size()` has reached the GC threshold.
    /// The threshold counts objects plus distinct interned strings and is
    /// checked before allocation.
    pub fn gc_should_run(&self) -> bool {
        self.heap.is_full()
    }

    /// Returns the current GC threshold in total heap allocations (objects plus
    /// distinct interned strings). Automatic thresholds are recomputed from the
    /// configured policy after collection; host-set thresholds stay fixed.
    pub fn gc_threshold(&self) -> usize {
        self.heap.threshold()
    }

    /// Sets the GC threshold in total heap allocations (objects plus distinct
    /// interned strings). Collection triggers before allocation when
    /// `heap_size() >= threshold`. This host-set threshold remains fixed across
    /// automatic and explicit collections.
    /// Set to `usize::MAX` to disable automatic GC. `gc_threshold()` reports
    /// `usize::MAX` while automatic collection is disabled.
    pub fn gc_set_threshold(&mut self, threshold: usize) {
        self.heap.set_threshold(threshold);
    }

    /// Replaces this State's automatic GC policy and resets its threshold to
    /// `initial_threshold`.
    ///
    /// After each automatic collection, the next threshold is
    /// `max(live_allocations * growth_factor, minimum_threshold)`. This also
    /// switches a manually set or disabled threshold back to automatic mode.
    /// `usize::MAX` as `initial_threshold` keeps automatic collection disabled.
    pub fn gc_set_policy(
        &mut self,
        initial_threshold: usize,
        minimum_threshold: usize,
        growth_factor: usize,
    ) {
        self.heap.set_policy(GcPolicy::new(
            initial_threshold,
            minimum_threshold,
            growth_factor,
        ));
    }

    /// Resumes the configured automatic policy at its initial threshold.
    ///
    /// The next collection then computes the threshold from the configured
    /// minimum and growth factor. If the policy's initial threshold is
    /// `usize::MAX`, automatic collection remains disabled.
    pub fn gc_set_auto_threshold(&mut self) {
        self.heap.reset_auto_threshold();
    }

    /// Disables automatic GC while preserving explicit `gc_collect()` calls.
    /// Explicit collections keep automatic GC disabled. Re-enable it with
    /// `gc_set_auto_threshold` to resume the configured policy or
    /// `gc_set_threshold` to use a fixed host threshold.
    pub fn gc_disable_auto(&mut self) {
        self.heap.disable_auto_gc();
    }

    /// Forces a full garbage collection cycle.
    /// This marks all reachable objects and frees unreachable ones.
    #[hotpath::measure]
    pub fn gc_collect(&mut self) {
        // This is the only production caller of GcHeap::collect. Heap unit
        // tests collect standalone heaps, which have no State pattern cache.
        // Clear before freeing interned strings so no cached matcher survives
        // its key's lifetime; keep this point fixed to preserve cache costs.
        self.gmatch_patterns.clear();
        self.gmatch_pattern_bytes = 0;
        // Mark all roots
        let mut worklist = self.heap.take_mark_worklist();
        mark_gc_roots(self, &mut worklist);
        // Open upvalues point at live stack slots but may not yet be held by
        // any closure (mid-capture in instr_closure); their pool slots are
        // roots for the pool sweep even though they hold no heap reference.
        for (_, uv_ref) in &self.open_upvalues {
            self.upvalue_pool.mark_slot(*uv_ref);
        }
        self.heap
            .drain_mark_worklist(&mut worklist, &self.upvalue_pool);
        self.heap.restore_mark_worklist(worklist);
        self.sweep_bytecode_caches();
        // Sweep unmarked objects
        self.heap.collect();
        // Pool slots not marked via a reachable closure or an open-upvalue
        // entry can never be read again; recycle them.
        self.upvalue_pool.sweep();
        // `format_pointer_ids` entries are deliberately not GC roots; an entry
        // whose value no longer resolves can never be observed again (the
        // generational key cannot alias a future allocation), so drop it to
        // keep identity storage bounded by live `%p` usage.
        self.format_pointer_ids
            .retain(|val, _| self.heap.val_is_live(val));
    }

    /// Run automatic collection before a caller proceeds with a heap
    /// allocation, using the caller's current roots.
    pub(super) fn maybe_collect(&mut self) {
        if self.heap.is_full() {
            self.gc_collect();
        }
    }

    /// Drop state-local runtime entries which no reachable closure, active
    /// frame, or pending closure shell can use. Removing an entry after roots
    /// were marked intentionally leaves its literals alive for one extra GC.
    /// Sweeping caches before marking would reclaim those literals sooner and
    /// change string-slot reuse and future collection thresholds.
    fn sweep_bytecode_caches(&mut self) {
        let mut live = BTreeSet::new();
        for bytecode in self.heap.reachable_lua_bytecodes() {
            Self::retain_bytecode_tree(&mut live, &bytecode);
        }
        for call in &self.call_stack {
            Self::retain_bytecode_tree(&mut live, &call.bytecode);
        }
        live.extend(self.pending_bytecode_caches.iter().copied());
        self.bytecode_caches
            .retain(|identity, _| live.contains(identity));
    }

    fn retain_bytecode_tree(live: &mut BTreeSet<usize>, bytecode: &Arc<Bytecode>) {
        if !live.insert(Arc::as_ptr(bytecode) as usize) {
            return;
        }
        for nested in &bytecode.nested {
            Self::retain_bytecode_tree(live, nested);
        }
    }

    /// Resolve the State-local runtime bundle. Prevalidation happens before
    /// mutating the string pool; each completed intern is transiently rooted so
    /// an allocation-triggered GC cannot collect an earlier literal.
    pub(super) fn resolve_bytecode_runtime(
        &mut self,
        bytecode: &Arc<Bytecode>,
    ) -> Result<Arc<BytecodeRuntime>> {
        let identity = Arc::as_ptr(bytecode) as usize;
        if let Some(entry) = self.bytecode_caches.get(&identity) {
            return Ok(Arc::clone(&entry.runtime));
        }
        for literal in &bytecode.string_literals {
            check_string_size(literal.len())?;
        }
        let watermark = self.transient_roots.values.len();
        let result = (|| {
            let mut literals = Vec::with_capacity(bytecode.string_literals.len());
            for literal in &bytecode.string_literals {
                let value = self.alloc_string(literal)?;
                self.transient_roots.values.push(value);
                literals.push(value);
            }
            let runtime = Arc::new(BytecodeRuntime {
                literals: literals.into_boxed_slice(),
                caches: RuntimeCaches::new(bytecode),
            });
            self.bytecode_caches.insert(
                identity,
                BytecodeCacheEntry {
                    bytecode: Arc::clone(bytecode),
                    runtime: Arc::clone(&runtime),
                },
            );
            Ok(runtime)
        })();
        self.transient_roots.values.truncate(watermark);
        result
    }

    #[cfg(feature = "snapshot")]
    pub(super) fn resolve_bytecode_runtime_no_gc(
        &mut self,
        bytecode: &Arc<Bytecode>,
    ) -> Arc<BytecodeRuntime> {
        let identity = Arc::as_ptr(bytecode) as usize;
        if let Some(entry) = self.bytecode_caches.get(&identity) {
            return Arc::clone(&entry.runtime);
        }
        let literals = bytecode
            .string_literals
            .iter()
            .map(|literal| Val::Str(self.heap.alloc_string(literal)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let runtime = Arc::new(BytecodeRuntime {
            literals,
            caches: RuntimeCaches::new(bytecode),
        });
        self.bytecode_caches.insert(
            identity,
            BytecodeCacheEntry {
                bytecode: Arc::clone(bytecode),
                runtime: Arc::clone(&runtime),
            },
        );
        runtime
    }

    // ========================================================================
    // Host callbacks
    // ========================================================================

    /// Called by the built-in `print()` function.
    /// Routes output through host callbacks with source context.
    pub(crate) fn host_print(&mut self, message: &str) {
        // Get current line from call stack (if available)
        let line = self
            .call_stack
            .last()
            .and_then(|info| {
                info.bytecode
                    .line_info
                    .get(info.ip.saturating_sub(1))
                    .copied()
            })
            .unwrap_or(0);

        let source = self
            .call_stack
            .last()
            .and_then(|info| info.bytecode.source.as_deref());
        self.callbacks.on_print(source, line, message);
    }

    /// Called when an error occurs. Notifies host callbacks.
    pub(crate) fn host_error(&mut self, error: &mut Error) {
        if error.mark_reported() {
            let source = self
                .call_stack
                .last()
                .and_then(|info| info.bytecode.source.as_ref());
            error.attach_source_name_if_absent(source);
            self.callbacks.on_error(error);
        }
    }

    /// Returns the active Lua frame source name, or `None` outside execution.
    pub fn current_source(&self) -> Option<&str> {
        self.call_stack
            .last()
            .and_then(|info| info.bytecode.source.as_deref())
    }

    /// Pushes onto the stack the value of the global `name`.
    #[hotpath::measure]
    pub fn get_global(&mut self, name: &str) -> Result<()> {
        let val = self.global_env.read(name, None);
        self.push_val(val)
    }

    /// Pops a value from the stack and sets it as the new value of global
    /// `name`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStackIndex` and leaves the stack unchanged when the
    /// current frame has no visible value; it never pops a slot owned by a
    /// calling Lua frame.
    #[hotpath::measure]
    pub fn set_global(&mut self, name: &str) -> Result<()> {
        if self.get_top() == 0 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: -1 }));
        }
        let val = self.pop_val();
        self.set_global_value(name, val);
        Ok(())
    }

    // Keep native context out of CallInfo: those entries represent Lua frames,
    // and widening them would add work and storage to every Lua call. Capture
    // only a function pointer at Rust dispatch; resolve names on errors only.
    // RustFn values carry no registration identity. Multiple global aliases
    // (or functions merged by LTO) therefore use the first installed global
    // still bound to that address; an unregistered callback stays anonymous.
    // Snapshot ids are a fallback, not names inferred from the Lua caller.
    pub(crate) fn active_rust_function_name(&self) -> Option<String> {
        let function = self.active_rust_fn?;
        for (name, value) in self.global_env.iter() {
            if let Val::RustFn(candidate) = value
                && *candidate as usize == function as usize
            {
                return Some(name.clone());
            }
        }
        #[cfg(feature = "snapshot")]
        if let Some(name) = self.rust_fn_ids_by_addr.get(&(function as usize)) {
            return Some(name.clone());
        }
        None
    }

    #[cfg(not(feature = "snapshot"))]
    pub(crate) fn set_global_rust_fn(&mut self, name: &str, func: RustFunc) {
        self.set_global_value(name, Val::RustFn(func));
    }

    /// Register a Rust function under a stable id and install it as a global.
    ///
    /// Save/load uses `id` to resolve reachable [`RustFunc`] values across
    /// processes; ids should be stable strings owned by the embedder.
    #[cfg(feature = "snapshot")]
    pub fn set_global_named_rust_fn(
        &mut self,
        name: &str,
        id: &str,
        func: RustFunc,
    ) -> std::result::Result<(), SaveError> {
        self.register_rust_fn(id, func)?;
        self.set_global_value(name, Val::RustFn(func));
        Ok(())
    }

    /// Converts a registration failure into an execution-API error, keeping
    /// the conflicting id of a duplicate registration structured.
    #[cfg(feature = "snapshot")]
    pub(crate) fn registration_error(err: SaveError) -> Error {
        match err {
            SaveError::DuplicateFunctionRegistration { id } => {
                Error::without_location(ErrorKind::DuplicateFunctionRegistration { id })
            }
            other => Error::without_location(ErrorKind::InternalError(other.to_string())),
        }
    }

    /// Register a Rust function id without installing the function anywhere.
    ///
    /// This is useful when the host pushes or stores the function manually but
    /// still wants it to survive save/load if Lua makes it reachable.
    /// Ids exceeding [`crate::limits::MAX_SAVE_IDENTIFIER_BYTES`] are rejected
    /// before either registry is changed.
    #[cfg(feature = "snapshot")]
    pub fn register_rust_fn(
        &mut self,
        id: &str,
        func: RustFunc,
    ) -> std::result::Result<(), save_state::SaveError> {
        if id.len() > crate::limits::MAX_SAVE_IDENTIFIER_BYTES {
            return Err(save_state::SaveError::TooLarge {
                what: "function id",
                count: id.len(),
            });
        }
        let addr = func as usize;
        // Keep SaveError in this public signature: load setup closures
        // propagate it. Execution-API wrappers convert it with
        // `registration_error`, which keeps the id structured. Moving the type
        // needs a coordinated API migration rather than a local change.
        // An id collision binds one id to two different function addresses
        // (an embedder bug). Re-registering the
        // same id->fn pair is idempotent.
        if let Some(existing_func) = self.rust_fns_by_id.get(id) {
            if *existing_func as usize != addr {
                return Err(save_state::SaveError::DuplicateFunctionRegistration {
                    id: id.to_string(),
                });
            }
            return Ok(());
        }
        self.rust_fns_by_id.insert(id.to_string(), func);
        // Several ids may legitimately share one address: intentional aliasing,
        // or identical-code folding collapsing distinct fns under release LTO.
        // Both are behaviorally safe (same code), so we never error here; we
        // keep the lexicographically-smallest id as the reverse name so saves
        // stay deterministic regardless of registration order.
        // Consequence: distinct host functions folded to one address both save
        // as the smallest id, so a build that does not fold them loads the
        // second as the first, and Lua == between them can differ from the
        // saving build. Embedders needing distinct identity must keep function
        // bodies observably distinct (as table.unpack keeps its own wrapper).
        // Refusing shared addresses would break aliasing and release LTO builds.
        match self.rust_fn_ids_by_addr.get(&addr) {
            Some(existing_id) if existing_id.as_str() <= id => {}
            _ => {
                self.rust_fn_ids_by_addr.insert(addr, id.to_string());
            }
        }
        Ok(())
    }

    pub(super) fn set_global_value(&mut self, name: &str, val: Val) {
        self.set_global_value_owned(name.to_string(), val);
    }

    pub(super) fn set_global_value_owned(&mut self, name: String, val: Val) {
        self.global_env.write(name, val);
    }

    /// Builds the shared string metatable with `__index` = the table at the
    /// top of the stack (the string library `open_string` just built) and
    /// installs it, replacing any earlier one. The stack is left unchanged; on
    /// error nothing is installed.
    pub(crate) fn install_string_metatable(&mut self) -> Result<()> {
        let lib = *self
            .stack
            .last()
            .ok_or_else(|| self.error(ErrorKind::InvalidStackIndex { index: -1 }))?;
        self.new_table_with_capacity(1)?;
        if let Err(e) = self.set_table_str_key_value(-1, "__index", lib) {
            self.pop_val();
            return Err(e);
        }
        let mt = self.pop_val();
        self.string_metatable = mt.as_object_ptr();
        self.string_mt_index_shape = None;
        Ok(())
    }

    /// Execute a function with a restricted global environment.
    /// Only globals in the whitelist are accessible during execution.
    /// The shared string metatable is not a global and is left in place, so
    /// `s:upper()` keeps working even when `"string"` is not whitelisted (as
    /// in reference Lua, where it reaches the library through that metatable).
    /// The original environment is restored after the function completes (or errors).
    pub fn with_restricted_env<F, R>(&mut self, whitelist: &[&str], f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        let saved = self.global_env.restrict(whitelist);
        self.transient_roots.suspended_envs.push(saved);

        // Execute the function under an unwind guard so a panic in `f` still
        // restores the original environment before propagating. Without
        // this, an embedder that catches the panic and reuses the State would
        // find it stuck in the restricted environment. (Under panic=abort
        // catch_unwind never returns, but reuse is moot there.)
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));

        // Restore original environment (on both normal return and unwind)
        let saved = self
            .transient_roots
            .suspended_envs
            .pop()
            .expect("suspended environment missing during restoration");
        self.global_env.replace(saved);

        match result {
            Ok(r) => r,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub(super) fn with_rooted_value<F, R>(&mut self, value: Val, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        self.with_rooted_values(&[value], f)
    }

    pub(super) fn with_rooted_values<F, R>(&mut self, values: &[Val], f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        // Preserve roots owned by any outer operation while this callback can
        // allocate, call Lua, or re-enter the host.
        let watermark = self.transient_roots.values.len();
        self.transient_roots.values.extend_from_slice(values);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.transient_roots.values.truncate(watermark);
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    /// Allocates a string on the heap.
    #[hotpath::measure]
    pub(super) fn alloc_string(&mut self, bytes: impl AsRef<[u8]>) -> Result<Val> {
        check_string_size(bytes.as_ref().len())?;
        self.maybe_collect();
        let ptr = self.heap.alloc_string(bytes.as_ref());
        Ok(Val::Str(ptr))
    }

    /// Construct an [`Error`] of the given kind with no source position.
    ///
    /// The position is filled in later, on the way out of the Lua frame the
    /// error surfaced in (see `locate_in_frame`), because that is the first
    /// point with access to the frame's line information. Errors that never
    /// unwind through a Lua frame - those raised by a host `RustFunc` called
    /// directly - keep a zero position.
    pub fn error(&self, kind: ErrorKind) -> Error {
        // Retain this public compatibility helper. Frame exit supplies the
        // location; locating here would confuse parser and caller locations.
        Error::without_location(kind)
    }

    pub(super) fn type_error(&self, e: TypeError) -> Error {
        self.error(ErrorKind::TypeError(e))
    }

    /// Build a stack trace from the current call stack and the active frame.
    /// The frame represents the innermost (current) function where the error occurred.
    #[allow(private_interfaces)]
    pub(super) fn build_stack_trace(&self, current_frame: &frame::Frame) -> Vec<StackFrame> {
        let mut trace = Vec::with_capacity(self.call_stack.len() + 1);

        // First entry: the current frame where the error occurred
        trace.push(current_frame.to_stack_frame());

        // Add entries from call_stack (most recent first).
        // Skip the last entry (current function) since we already have it from current_frame.
        // The remaining entries are the callers, with ip pointing to their call sites.
        for call_info in self.call_stack.iter().rev().skip(1) {
            // Get line number from the ip (call site)
            let line = if call_info.ip > 0 {
                call_info
                    .bytecode
                    .line_info
                    .get(call_info.ip - 1)
                    .copied()
                    .unwrap_or(0)
            } else {
                call_info.bytecode.line_info.first().copied().unwrap_or(0)
            };
            trace.push(StackFrame {
                function_name: call_info.bytecode.name.clone(),
                source: call_info.bytecode.source.as_deref().map(str::to_owned),
                line,
            });
        }

        trace
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn standard_library_initialization_populates_every_builtin_slot() {
        let state = State::new();
        assert!((0..Builtin::COUNT).all(|slot| state.global_env.builtin(slot as u8) != Val::Nil));
    }

    #[test]
    fn closures_of_one_bytecode_share_runtime_bundle() {
        let mut state = State::empty();
        let bytecode = Arc::new(Bytecode {
            string_literals: vec![b"field".to_vec()],
            ..Bytecode::default()
        });
        state
            .push_closure(Arc::clone(&bytecode), Vec::new())
            .expect("first closure allocates");
        state
            .push_closure(Arc::clone(&bytecode), Vec::new())
            .expect("second closure allocates");
        let second = state
            .pop_val()
            .as_lua_function(&state.heap)
            .expect("second value is a closure");
        let first = state
            .pop_val()
            .as_lua_function(&state.heap)
            .expect("first value is a closure");
        assert!(Arc::ptr_eq(&first.runtime, &second.runtime));
        assert_eq!(state.bytecode_caches.len(), 1);
    }

    #[test]
    fn warming_one_factory_closure_populates_the_shared_slots() {
        let mut state = State::new();
        state
            .load_string(
                "function make() return function(t) return t.v end end \
                 r1 = make() r2 = make() warmed = r1({ v = 3 })",
            )
            .expect("factory source compiles");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("factory source runs");
        state.get_global("r1").expect("r1 exists");
        let r1 = state
            .pop_val()
            .as_lua_function(&state.heap)
            .expect("r1 is a closure");
        state.get_global("r2").expect("r2 exists");
        let r2 = state
            .pop_val()
            .as_lua_function(&state.heap)
            .expect("r2 is a closure");
        assert!(Arc::ptr_eq(&r1.runtime, &r2.runtime));
        // r1 executed once; that warmup must be visible through r2's handle -
        // an actually-populated slot, not just pointer equality.
        assert!(
            r2.runtime
                .caches
                .field_lookup
                .iter()
                .any(|slot| slot.get_field().is_some()),
            "executing one closure must warm the shared field slot"
        );
    }

    #[test]
    fn runtime_cache_sweep_is_state_reachability_based() {
        let mut state = State::empty();
        state.gc_disable_auto();
        let bytecode = Arc::new(Bytecode {
            string_literals: vec![b"temporary".to_vec()],
            ..Bytecode::default()
        });
        state
            .push_closure(Arc::clone(&bytecode), Vec::new())
            .expect("closure allocates");
        state.pop_val();
        state.gc_collect();
        assert!(state.bytecode_caches.is_empty());
        // Removed entries were roots for the collection that removed them.
        assert_eq!(state.string_count(), 1);
        state.gc_collect();
        assert_eq!(state.string_count(), 0);
    }

    #[test]
    fn live_parent_retains_nested_runtime_entry() {
        let mut state = State::empty();
        state.gc_disable_auto();
        let child = Arc::new(Bytecode::default());
        let parent = Arc::new(Bytecode {
            nested: vec![Arc::clone(&child)],
            ..Bytecode::default()
        });
        state
            .push_closure(Arc::clone(&child), Vec::new())
            .expect("child closure allocates");
        state.pop_val();
        state
            .push_closure(Arc::clone(&parent), Vec::new())
            .expect("parent closure allocates");
        state.gc_collect();
        assert_eq!(state.bytecode_caches.len(), 2);
        state.pop_val();
        state.gc_collect();
        assert!(state.bytecode_caches.is_empty());
    }
}

#[cfg(test)]
mod tests;
