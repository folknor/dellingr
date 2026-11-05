//! This module provides the `State` struct, which handles the primary
//! components of the VM.

mod anchor;
#[cfg(all(test, feature = "disable-inline-caches"))]
mod cache_differential;
mod caches;
mod eval;
mod eval_control;
mod eval_index;
mod eval_store;
#[cfg(feature = "fault-injection")]
mod fault_injection;
mod frame;
mod global_env;
mod interrupt;
mod inventory;
mod lua_val;
mod meta_name;
mod metamethod;
mod object;
mod raw_field;
mod registers;
mod rng;
mod rust_fn;
#[cfg(feature = "snapshot")]
mod save_state;
mod stack;
mod string_arg;
mod table;
mod table_ops;

pub use anchor::Anchor;
#[cfg(feature = "fault-injection")]
pub use fault_injection::{FaultEvent, FaultSite};
pub use interrupt::Interrupt;
pub(crate) use interrupt::requested as interrupt_requested;
pub use lua_val::LuaType;
pub use lua_val::RustFunc;
pub(crate) use lua_val::number_to_string;
pub use raw_field::RawField;
pub use rust_fn::RustFnHandle;
pub(crate) use rust_fn::RustFnId;
use rust_fn::RustFnTable;
#[cfg(feature = "snapshot")]
pub use save_state::{
    CorruptionReason, DecodeReason, DuplicateEntry, GmatchCorruption, InvalidBytecodeReason,
    KeyForm, LoadError, MaterializedPart, SaveCollection, SaveDiagnostics, SaveError,
    SaveInternalError, SaveState, SavedReference, SnapshotError, SnapshotField, TableCorruption,
    TableLoadLocation, TableRestoreFailure,
};
pub(crate) use string_arg::StringArg;
pub(crate) use table_ops::TostringRendering;

use crate::det_hash::IndexMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use super::Instr;
use super::Result;
use super::compiler::Bytecode;
use super::cost_meter::{CostMeter, CostState};
use super::cost_model::{Charge, Costing};
use super::error::Error;
use super::error::ErrorKind;
use super::error::StackFrame;
use super::error::TypeError;
use super::host::{DefaultCallbacks, HostCallbacks};
use super::instr::{ArgCount, Builtin, RetCount};
use super::patterns::{AnchorMode, LuaPattern, SharedPattern};

use anchor::Registry;
use global_env::{EnvRole, GlobalEnv, SuspendedEnv};
use inventory::InFlight;
#[cfg(any(debug_assertions, feature = "snapshot"))]
use inventory::RestState;
pub(crate) use meta_name::MetaName;
use meta_name::MetaNames;

pub(super) use lua_val::PointerIdKey;
pub(super) use lua_val::Val;
pub(super) use object::ObjectPtr;
use object::{GcHeap, GcPolicy, StringPtr, UpvaluePool};
use rng::VmRng;
use table::Table;
pub(crate) use table::integer_index;

use crate::limits::{
    MAX_CALL_DEPTH, MAX_STACK_SIZE, PATTERN_CACHE_BYTES, PATTERN_CACHE_ENTRIES, check_string_size,
    tuning,
};
use caches::RuntimeCaches;
// Standard-library capacity counts stay beside their registration lists.
const STDLIB_NON_BUILTIN_GLOBALS: usize = crate::lua_std::STDLIB_NON_BUILTIN_GLOBALS;
const STDLIB_HEAP_TABLES: usize = crate::lua_std::STDLIB_HEAP_TABLES;
const STDLIB_INTERNED_TABLE_KEYS: usize = crate::lua_std::STDLIB_INTERNED_TABLE_KEYS;

struct PatternCacheEntry {
    pattern: SharedPattern,
    byte_len: usize,
}

/// One active Lua activation. `InFlight::call_stack` holds exactly one per
/// active Lua function call, innermost last, across every dispatch-loop
/// invocation; tracebacks, `print` locations and the bytecode-cache sweep
/// read it. The hot frame
/// state of an activation is not here: each dispatch-loop invocation keeps
/// its frames in a stack of its own (see `State::run_invocation`).
pub(super) struct CallInfo {
    /// The bytecode being executed.
    pub(super) bytecode: Arc<Bytecode>,
    /// Call-site snapshot: the instruction after the last one that took a
    /// path that may call out of this activation (see
    /// `State::record_call_site`). Not the live instruction pointer of the
    /// executing frame; while this activation is innermost it may be stale.
    pub(super) ip: usize,
    /// Start of this activation's extra arguments in `InFlight::vararg_stack`.
    /// The innermost activation owns the suffix, including an empty suffix.
    /// Call depth times MAX_ARGUMENTS bounds the arena below u32::MAX.
    pub(super) vararg_start: u32,
    /// Non-function `__call` chain steps resolved to reach this activation,
    /// and whether a tail call entered it. Each step holds one unit of
    /// metamethod_depth. Resolution transfers these for calls from Rust and
    /// Lua alike; popping the activation releases them before result
    /// adjustment, and so does a tail call the activation makes, which ends
    /// it (`State::tail_call_from_lua`).
    pub(super) chain: ChainUnits,
}

/// A `CallInfo`'s `__call` chain units with its tail-call mark in the top
/// bit: one word, so admission writes the record's last field with one
/// store. Separate `u16` and `bool` fields took two stores per call, and
/// fib-style recursion measured about 2.7% slower with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ChainUnits(u32);

// The units never reach the tail-call bit.
const _: () = assert!(crate::limits::MAX_METAMETHOD_DEPTH < ChainUnits::TAIL_CALLED);

impl ChainUnits {
    const TAIL_CALLED: u32 = 1 << 31;

    /// No units, not tail called: what admission records.
    pub(super) const NONE: Self = Self(0);

    /// Units held, the mark aside.
    pub(super) const fn units(self) -> u32 {
        self.0 & !Self::TAIL_CALLED
    }

    /// Whether a tail call entered the activation, replacing the one that
    /// made it. Tracebacks mark the frames that replacement removed.
    pub(super) const fn tail_called(self) -> bool {
        self.0 & Self::TAIL_CALLED != 0
    }

    /// The same units, marked as entered by a tail call.
    pub(super) const fn marked_tail_called(self) -> Self {
        Self(self.0 | Self::TAIL_CALLED)
    }

    /// Set the units to `units` (at most `MAX_METAMETHOD_DEPTH`), keeping
    /// the mark.
    pub(super) const fn with_units(self, units: u32) -> Self {
        Self(self.0 & Self::TAIL_CALLED | units)
    }
}

impl State {
    /// Out of line so the check adds nothing to `guarded`'s frame.
    #[cfg(debug_assertions)]
    #[cold]
    #[inline(never)]
    fn debug_assert_at_rest(&self) {
        debug_assert_ne!(self.rest_state(), RestState::Executing);
    }

    /// Refuse a snapshot unless only persistent state remains (see
    /// `State::rest_state`).
    #[cfg(feature = "snapshot")]
    fn validate_quiescent(&self) -> std::result::Result<(), save_state::SaveError> {
        #[cfg(feature = "fault-injection")]
        self.debug_assert_fault_injection_disarmed();
        if self.rest_state() == RestState::Quiescent {
            Ok(())
        } else {
            Err(save_state::SaveError::NotQuiescent)
        }
    }
}

/// State-local runtime data for one immutable bytecode chunk.
#[derive(Debug)]
struct BytecodeRuntime {
    literals: Box<[Val]>,
    caches: RuntimeCaches,
}

struct BytecodeCacheEntry {
    /// Pins the pointer-derived map identity against allocator ABA reuse.
    bytecode: Arc<Bytecode>,
    runtime: Arc<BytecodeRuntime>,
}

/// The main interface into the Lua VM.
pub struct State {
    #[cfg(feature = "fault-injection")]
    fault_injection: fault_injection::FaultInjection,
    /// The global table (which `_G` names), its builtin caches and its
    /// initialization phase.
    global_env: GlobalEnv,
    /// The metatable shared by every string, as in reference Lua. Built by
    /// `open_libs` with `__index` = the string library table it created, so
    /// string indexing (`s:upper()`) never consults the global `string`.
    /// `None` in a `State::raw` that never opened the libraries. Not a
    /// global: `with_restricted_env` leaves it alone. A GC root.
    pub(super) string_metatable: Option<ObjectPtr>,
    /// The slot of `__index` in `string_metatable`, recorded by the
    /// string-method IC's slow path so its fast path can re-check the
    /// `__index` value without a hash lookup. Reset when the metatable is
    /// replaced; a stale entry only causes misses.
    pub(super) string_mt_index_shape: Option<table::SlotRef>,
    /// Metamethod event keys, interned at construction and always rooted.
    meta_names: MetaNames,
    /// The main stack which stores values. Host-held: it may hold values the
    /// host pushed between calls, but must be empty for a snapshot.
    pub(super) stack: Vec<Val>,
    /// Execution-lifetime state: frame bases, activation records, varargs,
    /// open upvalues, transient roots and depths (see `inventory`).
    pub(super) in_flight: InFlight,
    /// The heap which holds any garbage-collected Objects.
    pub(super) heap: GcHeap,
    /// State-local runtime data, keyed by the pinned Bytecode allocation.
    bytecode_caches: IndexMap<usize, BytecodeCacheEntry>,
    /// Environments suspended by `with_restricted_env`, innermost last.
    /// Host-held, like `stack`: non-empty while a host callback runs inside
    /// a restricted environment, refused by a snapshot.
    suspended_envs: Vec<SuspendedEnv>,
    /// Pool for upvalue storage. Avoids per-upvalue heap allocations.
    pub(super) upvalue_pool: UpvaluePool,
    /// Runtime cost accounting and optional budget.
    pub(super) cost: CostState,
    /// The host's request to stop running Lua, polled at safepoints (see
    /// `vm/interrupt.rs`). Host state: never saved, never a root.
    pub(super) interrupt: Interrupt,
    /// Compiled pattern programs shared by `string.find`, `string.match`,
    /// `string.gsub` and `string.gmatch` iterators.
    ///
    /// Keys are interned string pointers, so cache lookup does not compare
    /// pattern bytes, paired with the effective `AnchorMode`: `gmatch` reads
    /// a leading `^` literally where the other functions anchor, so the same
    /// string can need two programs. Insertion order makes eviction
    /// deterministic.
    /// It is an implementation cache only: no charge depends on residency,
    /// iterator state remains entirely in Lua values, and a snapshot simply
    /// rebuilds entries on later use. Entries are reference-counted, so an
    /// operation that calls back into Lua (a gsub replacement function) keeps
    /// its program alive across GC pruning or eviction.
    patterns: IndexMap<(StringPtr, AnchorMode), PatternCacheEntry>,
    pattern_bytes: usize,
    /// Test instrumentation: how many patterns the cache has compiled.
    #[cfg(test)]
    pattern_compilations: u64,
    /// Set once a panic has unwound through VM execution. A panic skips every
    /// pop and restore on its way out, so `in_flight` no longer describes
    /// the live frames; a poisoned State refuses further use (see
    /// `ErrorKind::StatePoisoned`). Never cleared.
    pub(super) poisoned: bool,
    /// Spare frame stacks for dispatch-loop invocations, so a re-entry
    /// allocates none in steady state. Always empty stacks: an invocation
    /// takes one, and gives it back once its frames have all returned. Holds
    /// no VM values (frames own only `Arc`s), so it is no GC root, and it is
    /// capacity rather than state, so a snapshot ignores it.
    pub(in crate::vm) frame_stacks: Vec<Vec<frame::Frame>>,
    /// Host callbacks for print output, error handling, etc.
    pub(super) callbacks: Box<dyn HostCallbacks>,
    /// User-defined data that RustFuncs can access.
    /// Use `set_user_data<T>()` and `user_data<T>()` to store/retrieve.
    user_data: Option<Box<dyn std::any::Any + Send>>,
    /// Seeded RNG for deterministic math.random(). Defaults to seed 0.
    /// Use `set_rng_seed()` to set a specific seed for replay.
    pub(super) rng: VmRng,
    /// Registry of values retained from Rust via `Anchor` handles. Acts as
    /// an additional GC root set (`GcRoot::Anchors`). Carries
    /// the State's process-unique `state_id` so cross-State misuse of an
    /// `Anchor` is caught.
    pub(super) registry: Registry,
    /// Lazily assigned deterministic identities used by `string.format("%p")`.
    /// These values deliberately are not GC roots; every collection drops the
    /// entries of values it freed, so a later allocation cannot alias an
    /// identity belonging to a freed value.
    // Insertion order is serialized. Do not prune outside gc_collect: changing
    // which entries remain must not change snapshot traversal order. With GC
    // disabled, entries grow with observed objects, just as the heap does.
    pub(super) format_pointer_ids: IndexMap<PointerIdKey, u64>,
    pub(super) next_format_pointer_id: u64,
    /// Every Rust function a value of this State can hold: the standard
    /// library catalog, then the host's entries, with their stable save ids
    /// under `snapshot` (see `vm/rust_fn.rs`). Entries persist even while
    /// unreachable: the host may push a handle again, and snapshots must
    /// still resolve its stable id. An unregister API would need an explicit
    /// host lifetime contract.
    pub(super) rust_fns: RustFnTable,
}

/// Unwind payload that abandons execution on a State poisoned by a panic a
/// host callback caught and swallowed. Unwinding, rather than returning an
/// error, is what keeps the still-live outer Lua frames from running their
/// ordinary cleanup (stack truncation, upvalue closing, root pops) against
/// bookkeeping the swallowed panic left inconsistent. `State::guarded`
/// turns it into `ErrorKind::StatePoisoned` at the host boundary.
pub(crate) struct PoisonUnwind;

// Important note on how the stack is tracked:
// A State uses a single stack for all local variables, temporary values,
// function arguments, and function return values. Both Lua frames and Rust
// frames use this stack. `self.in_flight.stack_bottom` refers to the first value in the
// stack which belongs to the current frame. Note that Rust functions access
// the stack using 1-based indexing, but Lua code uses 0-based indexing.

/// Builds a [`State`]: host callbacks, garbage-collection policy, whether the
/// standard library is opened, and how cost is accounted. Every setting has
/// a default, so `State::builder().build()` is [`State::new`].
///
/// # Example
///
/// ```
/// use dellingr::{Charge, CostModel, Costing, HostCallbacks, State};
///
/// struct MyCallbacks {
///     output: Vec<Vec<u8>>,
/// }
/// impl HostCallbacks for MyCallbacks {
///     fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
///         self.output.push(message.to_vec());
///     }
/// }
///
/// let state = State::builder()
///     .callbacks(Box::new(MyCallbacks { output: vec![] }))
///     .costing(Costing::Metered(CostModel::default().with(Charge::Pow, 5)))
///     .build();
/// assert_eq!(state.cost_used(), 0);
/// ```
#[must_use]
pub struct StateBuilder {
    callbacks: Option<Box<dyn HostCallbacks>>,
    gc_policy: GcPolicy,
    standard_library: bool,
    costing: Costing,
    interrupt: Option<Interrupt>,
}

impl StateBuilder {
    fn new() -> Self {
        Self {
            callbacks: None,
            gc_policy: GcPolicy::default(),
            standard_library: true,
            costing: Costing::default(),
            interrupt: None,
        }
    }

    /// The [`Interrupt`] the State polls. By default it gets a fresh one of
    /// its own; passing a clone of a host-held handle lets one trigger stop
    /// every State that shares it.
    pub fn interrupt(mut self, interrupt: Interrupt) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    /// Host callbacks. The default prints to stdout ([`DefaultCallbacks`]).
    pub fn callbacks(mut self, callbacks: Box<dyn HostCallbacks>) -> Self {
        self.callbacks = Some(callbacks);
        self
    }

    /// A host-selected garbage collection policy.
    ///
    /// The initial threshold is active while the standard library is opened.
    /// After a collection while automatic mode is active, the next threshold is
    /// `max(live_allocations * growth_factor, minimum_threshold)`. These values
    /// are runtime policy and are not written to snapshots. A calculated
    /// `usize::MAX` remains automatic and is recomputed by later collections.
    ///
    /// `usize::MAX` as `initial_threshold` disables automatic collection until
    /// the threshold is changed. A zero threshold triggers collection before
    /// each allocation. The default policy is `(20, 20, 2)`.
    pub fn gc_policy(
        mut self,
        initial_threshold: usize,
        minimum_threshold: usize,
        growth_factor: usize,
    ) -> Self {
        self.gc_policy = GcPolicy::new(initial_threshold, minimum_threshold, growth_factor);
        self
    }

    /// Build without opening the standard library (see [`State::raw`]).
    pub fn raw(mut self) -> Self {
        self.standard_library = false;
        self
    }

    /// How the State accounts for cost: metered by a [`crate::CostModel`] (the
    /// default model by default) or [`Costing::Off`]. Fixed for the State's
    /// whole life.
    pub fn costing(mut self, costing: Costing) -> Self {
        self.costing = costing;
        self
    }

    /// Builds the State. Opening the standard library charges nothing under
    /// any costing.
    #[must_use]
    pub fn build(self) -> State {
        let callbacks = self.callbacks.unwrap_or_else(|| Box::new(DefaultCallbacks));
        let mut state = State::build_raw(callbacks, self.gc_policy, self.costing);
        if let Some(interrupt) = self.interrupt {
            state.interrupt = interrupt;
        }
        if self.standard_library {
            state
                .open_libs_unchecked()
                .expect("standard library initialization starts with an empty value stack");
        }
        state
    }
}

impl State {
    /// Creates a new, independent state with default callbacks (stdout), the
    /// standard library and the default cost model. See [`State::builder`]
    /// for the other settings.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// A builder for a State with non-default settings.
    pub fn builder() -> StateBuilder {
        StateBuilder::new()
    }

    /// Record the environment tables right after `open_libs`, before any user
    /// code can shadow a builtin slot (`GlobalEnv::adopt_env_tables`). The
    /// seal and the snapshot (object -> token on save, token -> object on
    /// load) both go through this record, never the live builtin slots, so
    /// `math = {}` cannot make a user table an environment table.
    pub(crate) fn capture_env_tables(&mut self) {
        let mut tables: Vec<(ObjectPtr, EnvRole)> = Vec::new();
        // One object holding two roles keeps the last one captured.
        let mut add = |ptr: ObjectPtr, role: EnvRole| {
            tables.retain(|(existing, _)| *existing != ptr);
            tables.push((ptr, role));
        };
        for (builtin, role) in global_env::LIBRARY_BUILTINS {
            if let Val::Obj(ptr) = self.global_env.read_builtin(&self.heap, builtin.to_u8()) {
                add(ptr, role);
            }
        }
        // The global table itself is not an environment table: it exists in
        // every State, raw ones included. Its protected metatable is rebuilt
        // by `open_libs`.
        if let Some(mt) = self
            .heap
            .as_table_ref(self.global_env.table())
            .and_then(Table::get_metatable)
        {
            add(mt, EnvRole::GlobalMetatable);
        }
        // The shared string metatable is rebuilt by `open_libs` too, so a
        // script holding it (`local mt = getmetatable("")`) reloads it as the
        // live object.
        if let Some(mt) = self.string_metatable {
            add(mt, EnvRole::StringMetatable);
        }
        self.global_env.adopt_env_tables(&mut self.heap, tables);
    }

    /// Creates a raw state, without opening any of the standard libraries:
    /// its global namespace is entirely empty, as with `lua_newstate` in the
    /// reference C API. [`State::open_libs`] installs them later if wanted.
    /// [`StateBuilder::raw`] builds one with other settings.
    pub fn raw() -> Self {
        Self::builder().raw().build()
    }

    /// The State every constructor starts from: the GC policy is active from
    /// the first allocation, the costing is fixed, and nothing is opened.
    fn build_raw(callbacks: Box<dyn HostCallbacks>, gc_policy: GcPolicy, costing: Costing) -> Self {
        let state_id = anchor::next_state_id();
        let mut heap = GcHeap::with_policy(gc_policy);
        let meta_names = MetaNames::intern(&mut heap);
        let global_env = GlobalEnv::new(&mut heap);
        Self {
            global_env,
            string_metatable: None,
            string_mt_index_shape: None,
            meta_names,
            stack: Vec::with_capacity(tuning::INITIAL_VM_STACK_CAPACITY),
            in_flight: InFlight::new(),
            heap,
            bytecode_caches: IndexMap::default(),
            suspended_envs: Vec::new(),
            upvalue_pool: UpvaluePool::new(),
            cost: CostState::new(costing),
            interrupt: Interrupt::new(),
            patterns: IndexMap::default(),
            pattern_bytes: 0,
            #[cfg(test)]
            pattern_compilations: 0,
            poisoned: false,
            frame_stacks: Vec::new(),
            callbacks,
            user_data: None,
            #[cfg(feature = "fault-injection")]
            fault_injection: fault_injection::FaultInjection::default(),
            rng: VmRng::seed_from_u64(VmRng::DEFAULT_SEED),
            registry: Registry::new(state_id),
            format_pointer_ids: IndexMap::default(),
            next_format_pointer_id: 1,
            rust_fns: RustFnTable::new(state_id),
        }
    }

    pub(crate) fn reserve_stdlib_capacity(&mut self) {
        self.global_env.reserve(
            &mut self.heap,
            Builtin::COUNT + STDLIB_NON_BUILTIN_GLOBALS + tuning::HOST_GLOBAL_CAPACITY_HEADROOM,
        );
        self.heap
            .reserve(STDLIB_HEAP_TABLES, STDLIB_INTERNED_TABLE_KEYS);
    }

    /// Sets the RNG seed for deterministic math.random() behavior.
    ///
    /// Adding `k * 0x9e3779b97f4a7c15` to a seed, with wrapping `u64`
    /// arithmetic, shifts the SplitMix64 stream by `k` draws. Distinct seeds
    /// chosen this way therefore produce overlapping streams, not independent
    /// per-entity streams.
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
    ///
    /// # Errors
    ///
    /// [`ErrorKind::CostingOff`] when the State was built with
    /// [`Costing::Off`], and the usual refusal of a poisoned State; nothing
    /// changes.
    pub fn set_cost_budget(&mut self, budget: i64) -> Result<()> {
        self.check_usable()?;
        if self.cost.set_budget(budget) {
            Ok(())
        } else {
            Err(self.error(ErrorKind::CostingOff))
        }
    }

    /// Returns the total cost consumed since the last budget reset. Always 0
    /// when costing is off.
    pub fn cost_used(&self) -> u64 {
        self.cost.used()
    }

    /// Returns the cost remaining in the budget.
    /// Can be negative if the last action pushed over budget.
    /// Without a configured budget, and when costing is off, returns the
    /// unbounded sentinel `i64::MAX`.
    pub fn cost_remaining(&self) -> i64 {
        self.cost.remaining()
    }

    /// How this State accounts for cost, fixed when it was built (see
    /// [`StateBuilder::costing`]).
    pub fn costing(&self) -> &Costing {
        self.cost.costing()
    }

    /// The [`Interrupt`] this State polls. Clone it to stop the State from
    /// another thread or a signal handler while it runs Lua.
    #[must_use]
    pub fn interrupt(&self) -> Interrupt {
        self.interrupt.clone()
    }

    /// Poll `interrupt` from now on instead of the current handle, for
    /// example to join a State restored by `load_state` to a host-wide
    /// handle. Allowed at any time, from a Rust callback too: the next
    /// safepoint reads the new handle, so a request set only on the old one
    /// no longer applies. Stickiness belongs to the handle, not to a run.
    pub fn set_interrupt(&mut self, interrupt: Interrupt) {
        self.interrupt = interrupt;
    }

    /// A safepoint: fail with [`ErrorKind::Interrupted`] while the host's
    /// interrupt is triggered (see `vm/interrupt.rs` for where they are).
    #[inline(always)]
    pub(crate) fn poll_interrupt(&self) -> Result<()> {
        if interrupt::requested(self.interrupt.flag()) {
            return Err(self.interrupted_error());
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn interrupted_error(&self) -> Error {
        self.error(ErrorKind::Interrupted)
    }

    /// Test instrumentation for the compiled-pattern cache.
    #[cfg(test)]
    pub(crate) fn pattern_compilations(&self) -> u64 {
        self.pattern_compilations
    }

    /// Consume cost from the budget. Returns an error if budget is exhausted
    /// and cost > 0. The action that pushes you over budget completes before
    /// stopping (checked at the START of each operation).
    ///
    /// Use this in RustFuncs to charge for expensive operations. The cost is
    /// raw: the State's [`crate::CostModel`] does not weigh it. When costing is off
    /// it is admitted without being counted.
    #[inline(always)]
    pub fn consume_cost(&mut self, cost: u64) -> Result<()> {
        self.check_usable()?;
        self.charge_cost(cost)
    }

    /// [`State::consume_cost`] without the poison check: the dispatch loop's
    /// cost settlement uses this, so poisoning adds nothing per opcode.
    #[inline(always)]
    pub(crate) fn charge_cost(&mut self, cost: u64) -> Result<()> {
        if self.cost_meter().consume(cost) {
            Ok(())
        } else {
            Err(self.budget_exceeded_error())
        }
    }

    /// Returns a meter for runtime work that does not need VM state, charging
    /// raw cost (unit cost 1). Library work charges through
    /// [`State::charge_meter`] instead, so its registry weight applies.
    pub(crate) fn cost_meter(&mut self) -> CostMeter<'_> {
        self.cost.meter()
    }

    /// Charge `units` units of `charge` as one operation: like
    /// [`State::consume_cost`] with the weighted cost, so the charge that
    /// crosses the budget completes.
    pub(crate) fn charge(&mut self, charge: Charge, units: u64) -> Result<()> {
        self.consume_cost(self.cost.cost(charge, units))
    }

    /// Charge `units` units of `charge` one unit at a time (see
    /// [`CostMeter::consume_units`]): the charge stops at the first unit
    /// that starts with no budget left, as the per-unit loop it batches would.
    pub(crate) fn charge_units(&mut self, charge: Charge, units: u64) -> Result<()> {
        if self.charge_meter(charge).consume_units(units) {
            Ok(())
        } else {
            Err(self.budget_exceeded_error())
        }
    }

    /// A meter whose unit costs the weight of `charge`, for work that charges
    /// as it runs without VM state (the pattern matcher).
    pub(crate) fn charge_meter(&mut self, charge: Charge) -> CostMeter<'_> {
        self.cost.charge_meter(charge)
    }

    /// Flush before batching would hide exhaustion of a configured budget.
    pub(super) fn cost_batch_needs_flush(&self, pending: u64) -> bool {
        self.cost.batch_needs_flush(pending)
    }

    /// The compiled program for the interned pattern string at stack slot `idx`,
    /// compiling and retaining it on a cache miss.
    ///
    /// No caller charges for compilation itself, so residency is invisible to
    /// the cost model. Malformed pattern points are encoded in the program and
    /// raise only when a match reaches them, cached or not.
    fn cached_pattern_at(&mut self, idx: usize, mode: AnchorMode) -> SharedPattern {
        let val = &self.stack[idx];
        let ptr = val
            .as_string_ptr()
            .expect("expect_string_at validated the value's string type");
        let bytes = val
            .as_string(&self.heap)
            .expect("string pointer came from a string value");
        let key = (ptr, mode.effective(bytes));
        if let Some(entry) = self.patterns.get(&key) {
            return entry.pattern.clone();
        }
        let pattern_len = bytes.len();
        let compiled = SharedPattern::compile(bytes, key.1);
        #[cfg(test)]
        {
            self.pattern_compilations = self.pattern_compilations.saturating_add(1);
        }
        // The cache is only an optimization. An entry that cannot fit by
        // itself must bypass it rather than evicting every cached pattern.
        if PATTERN_CACHE_ENTRIES == 0 || pattern_len > PATTERN_CACHE_BYTES {
            return compiled;
        }
        while self.patterns.len() >= PATTERN_CACHE_ENTRIES
            || self.pattern_bytes.saturating_add(pattern_len) > PATTERN_CACHE_BYTES
        {
            let (_, evicted) = self
                .patterns
                .shift_remove_index(0)
                .expect("a nonempty pattern cache has an oldest entry");
            self.pattern_bytes = self.pattern_bytes.saturating_sub(evicted.byte_len);
        }
        self.pattern_bytes = self.pattern_bytes.saturating_add(pattern_len);
        self.patterns.insert(
            key,
            PatternCacheEntry {
                pattern: compiled.clone(),
                byte_len: pattern_len,
            },
        );
        compiled
    }

    /// A matcher for the string pattern at 1-based stack index `idx`, from
    /// cache, with a leading `^` anchoring the match.
    ///
    /// The matcher owns a reference to its program, so the caller may run Lua
    /// (and so allocate, collect or evict) while holding it.
    pub(crate) fn cached_pattern_matcher(&mut self, idx: isize) -> Result<LuaPattern> {
        let idx = self.expect_string_at(idx)?;
        Ok(self.cached_pattern_at(idx, AnchorMode::Honour).matcher())
    }

    /// Run a gmatch search with the cached matcher for its pattern, in which
    /// a leading `^` is a literal byte, as in reference `gmatch`.
    pub(crate) fn with_gmatch_matcher<T>(
        &mut self,
        subject_idx: isize,
        pattern_idx: isize,
        operation: impl FnOnce(
            &[u8],
            &mut LuaPattern,
            CostMeter<'_>,
            &std::sync::atomic::AtomicBool,
        ) -> T,
    ) -> Result<T> {
        // Preserve the argument-type contract before touching the cache.
        self.expect_string_at(subject_idx)?;
        let pattern_idx = self.expect_string_at(pattern_idx)?;
        let mut matcher = self
            .cached_pattern_at(pattern_idx, AnchorMode::Literal)
            .matcher();
        self.with_string_arg(
            &StringArg::Slot(subject_idx),
            Charge::StringGmatch,
            |subject, meter, interrupt| operation(subject, &mut matcher, meter, interrupt),
        )
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
    /// ```
    /// use dellingr::{ArgCount, Program, RetCount, State};
    ///
    /// #[derive(Default)]
    /// struct CommandCollector {
    ///     turns: Vec<f64>,
    /// }
    ///
    /// let mut state = State::new();
    /// state.set_user_data(CommandCollector::default());
    ///
    /// // A RustFunc is a plain function pointer, so the callback reaches the
    /// // collector through the State rather than by capturing it.
    /// let turn = state.add_rust_fn("turn", |state| {
    ///     let turn = state.to_number(1)?;
    ///     state
    ///         .user_data_mut::<CommandCollector>()
    ///         .expect("the host stores a CommandCollector before running scripts")
    ///         .turns
    ///         .push(turn);
    ///     Ok(0)
    /// })?;
    /// state.push_rust_fn(turn)?;
    /// state.set_global("turn")?;
    ///
    /// let program = Program::compile("turn(0.5) turn(-0.25)", None)?;
    /// state.load(&program)?;
    /// state.call(ArgCount::Fixed(0), RetCount::Fixed(0))?;
    ///
    /// let collector = state
    ///     .user_data::<CommandCollector>()
    ///     .expect("the collector is still stored");
    /// assert_eq!(collector.turns, [0.5, -0.25]);
    /// # Ok::<(), dellingr::error::Error>(())
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
    /// After each collection while automatic mode is active, the next threshold is
    /// `max(live_allocations * growth_factor, minimum_threshold)`. This also
    /// switches a manually set or disabled threshold back to automatic mode.
    /// A calculated `usize::MAX` remains automatic and is recomputed by later
    /// collections.
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

    /// Drop state-local runtime entries which no reachable closure, active
    /// frame, or pending closure shell can use. Removing an entry after roots
    /// were marked leaves its literals alive for one extra GC. Sweeping caches
    /// before marking would reclaim them sooner; collection timing is not a
    /// cross-version contract (`docs/determinism.md`), so that is allowed as
    /// a called-out change.
    ///
    /// `live` arrives holding what the root visit retained
    /// (`GcRoot::ActiveChunk` trees and `GcRoot::PendingChunk` entries). The
    /// marked closures are known only once marking has finished: tracing a
    /// closure flags its runtime as reached (`GcHeap::mark_children`), and
    /// every closure's runtime is its bytecode's entry here, so the flagged
    /// entries are exactly the bytecodes of the marked closures. They are
    /// added here, before the pending identities.
    fn sweep_bytecode_caches(&mut self, live: object::LiveChunks) {
        // Bytecode identity deduplication also covers nested prototypes and
        // active/pending functions with no reachable heap closure. Keep one
        // ordered set for all three sources rather than treating the heap's
        // marked closures as the complete runtime-cache root set.
        let object::LiveChunks {
            trees: mut live,
            pending,
        } = live;
        for entry in self.bytecode_caches.values() {
            // Take every flag, even for an entry already retained through a
            // tree, so none is left set for the next collection.
            if entry.runtime.caches.take_reached() {
                Self::retain_bytecode_tree(&mut live, &entry.bytecode);
            }
        }
        live.extend(pending);
        self.bytecode_caches
            .retain(|identity, _| live.contains(identity));
    }

    /// Clear every runtime cache entry that names an object the sweep freed
    /// (see `RuntimeCaches::drop_dead_entries`). Runs after
    /// `sweep_bytecode_caches` and the heap sweep, on every
    /// `object::CACHE_PRUNE_INTERVAL`-th collection; that constant has the
    /// argument for why the gap is safe. The map holds every runtime that
    /// can execute again: each runtime is created into it, closures and
    /// frames take theirs from it, and an entry is dropped only once no
    /// surviving closure, activation or pending closure shell uses its
    /// bytecode. Debug builds check the closure half on every collection
    /// (`check_closure_runtimes`).
    #[inline(never)]
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    fn drop_dead_cache_entries(&self) {
        for entry in self.bytecode_caches.values() {
            entry.runtime.caches.drop_dead_entries(&self.heap);
        }
    }

    /// Debug check, after every collection's sweep: each surviving closure's
    /// runtime is the cached runtime for its bytecode. The cache prune and
    /// the mark phase both rely on it: the prune reaches a closure's caches
    /// only through the map, and marking roots a runtime's literals only
    /// through it (`GcRoot::ChunkLiterals`), never per closure.
    #[cfg(debug_assertions)]
    fn check_closure_runtimes(&self) {
        for closure in self.heap.closures() {
            let identity = Arc::as_ptr(&closure.bytecode) as usize;
            debug_assert!(
                self.bytecode_caches
                    .get(&identity)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.runtime, &closure.runtime)),
                "a surviving closure's runtime is not the cached runtime for its bytecode, \
                 so weak cache processing would miss it"
            );
        }
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
    fn resolve_bytecode_runtime(
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
        let watermark = self.in_flight.rooted_values.len();
        let result = (|| {
            let mut literals = Vec::with_capacity(bytecode.string_literals.len());
            for literal in &bytecode.string_literals {
                let value = self.alloc_string(literal)?;
                self.in_flight.rooted_values.push(value);
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
        self.in_flight.rooted_values.truncate(watermark);
        result
    }

    #[cfg(feature = "snapshot")]
    fn resolve_bytecode_runtime_no_gc(&mut self, bytecode: &Arc<Bytecode>) -> Arc<BytecodeRuntime> {
        let identity = Arc::as_ptr(bytecode) as usize;
        if let Some(entry) = self.bytecode_caches.get(&identity) {
            return Arc::clone(&entry.runtime);
        }
        let literals = bytecode
            .string_literals
            .iter()
            .map(|literal| Val::Str(self.heap.no_gc_allocator().alloc_string(literal)))
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

    /// Prefer the script caller over the private `string.gmatch` shim when
    /// reporting source context. Keep the shim when it has no script caller.
    /// Takes the call stack rather than `&self` so the returned borrow stays
    /// disjoint from `self.callbacks`.
    fn host_context_frame(call_stack: &[CallInfo]) -> Option<&CallInfo> {
        let last = call_stack.last()?;
        if !Self::is_gmatch_wrapper(&last.bytecode) {
            return Some(last);
        }
        call_stack
            .iter()
            .rev()
            .skip(1)
            .find(|info| !Self::is_gmatch_wrapper(&info.bytecode))
            .or(Some(last))
    }

    /// Called by the built-in `print()` function.
    /// Routes output, byte-exact, through host callbacks with source context.
    pub(crate) fn host_print(&mut self, message: &[u8]) {
        // Get current line from call stack (if available)
        let frame = Self::host_context_frame(&self.in_flight.call_stack);
        let line = frame
            .and_then(|info| {
                info.bytecode
                    .line_info
                    .get(info.ip.saturating_sub(1))
                    .copied()
            })
            .unwrap_or(0);

        let source = frame.and_then(|info| info.bytecode.source.as_deref());
        self.callbacks.on_print(source, line, message);
    }

    /// Called when an error occurs. Notifies host callbacks.
    pub(crate) fn host_error(&mut self, error: &mut Error) {
        if error.mark_reported() {
            // Gmatch implementation chunks are omitted when a script caller
            // can provide source context; a host-invoked shim keeps its own.
            let source = Self::host_context_frame(&self.in_flight.call_stack)
                .and_then(|info| info.bytecode.source.as_ref());
            error.attach_source_name_if_absent(source);
            self.callbacks.on_error(error);
        }
    }

    /// Returns the active script caller's chunk name, byte-exact, or `None`
    /// outside execution or in an unnamed chunk. The private `string.gmatch`
    /// shim is skipped when a script caller exists.
    pub fn current_source(&self) -> Option<&[u8]> {
        Self::host_context_frame(&self.in_flight.call_stack)
            .and_then(|info| info.bytecode.source.as_deref())
    }

    /// Pushes onto the stack the value of the global `name`.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn get_global(&mut self, name: &str) -> Result<()> {
        self.check_usable()?;
        let val = self.global_env.read_name(&self.heap, name);
        self.push_val(val)
    }

    /// Pushes the global table, which `open_libs` binds to `_G`.
    pub(crate) fn push_global_table(&mut self) -> Result<()> {
        self.push_val(Val::Obj(self.global_env.table()))
    }

    /// Pops a value from the stack and sets it as the new value of global
    /// `name`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStackIndex` and leaves the stack unchanged when the
    /// current frame has no visible value; it never pops a slot owned by a
    /// calling Lua frame.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub fn set_global(&mut self, name: &str) -> Result<()> {
        self.check_usable()?;
        self.pop_to_global(name)
    }

    /// [`State::set_global`] without the entry check, for library setup.
    pub(crate) fn pop_to_global(&mut self, name: &str) -> Result<()> {
        if self.get_top() == 0 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: -1 }));
        }
        // Intern the name while the value is still on the stack: interning
        // can collect, and the stack slot is the value's only root.
        let key = self.global_name_key(name, self.stack[self.stack.len() - 1])?;
        let val = self.pop_val();
        if let Some(key) = key {
            self.global_env.write_key(&mut self.heap, key, val);
        }
        Ok(())
    }

    /// The interned key for global `name` before storing `val` under it, or
    /// `None` when there is nothing to store: a nil write to a name nothing
    /// has interned cannot change a binding, so it allocates no string.
    /// Interning may collect, so `val` must already be rooted unless it is a
    /// value the collector never frees.
    fn global_name_key(&mut self, name: &str, val: Val) -> Result<Option<StringPtr>> {
        if matches!(val, Val::Nil) {
            return Ok(self.heap.find_string(name.as_bytes()));
        }
        check_string_size(name.len())?;
        Ok(Some(self.allocator().alloc_string(name.as_bytes())))
    }

    /// Ends initialization of the current global environment.
    ///
    /// Until sealed, every global keeps its slot in the global table for the
    /// life of the State once it has held a value, even after it is set to
    /// nil. After sealing, globals set to nil are reclaimed as in any table,
    /// so a script that writes computed names through `_G` stays bounded;
    /// with the `snapshot` feature, sealing records the names written until
    /// then, so a load still keeps a deleted one deleted. The library tables
    /// `open_libs` built (`math`, `string`, `table`, the global and string
    /// metatables) keep the same rule and are sealed with the globals.
    ///
    /// Call it once the host has installed its environment (libraries, host
    /// functions, any setup Lua) and before gameplay scripts run. With the
    /// `snapshot` feature, a `load_state` setup must replay the same
    /// initialization the saving State performed before it sealed; the loaded
    /// State takes the saved State's sealed or unsealed phase, and a call to
    /// this method inside that setup is deferred to it. Sealing is one-way;
    /// calling it again does nothing within that environment.
    ///
    /// Restricted environments created by [`State::with_restricted_env`] start
    /// sealed. Calling this method inside one does nothing and does not seal
    /// the suspended outer environment; its phase is restored on return.
    pub fn seal_globals(&mut self) {
        self.global_env.seal(&mut self.heap);
    }

    /// Whether the current global environment is sealed.
    ///
    /// Always true inside [`State::with_restricted_env`], including when the
    /// suspended outer environment is still initializing.
    pub fn globals_sealed(&self) -> bool {
        self.global_env.is_sealed()
    }

    // Keep native context out of CallInfo: those entries represent Lua frames,
    // and widening them would add work and storage to every Lua call. Capture
    // only the function's handle at Rust dispatch; resolve names on errors
    // only. The name is the entry's own, fixed when it was created; it is not
    // discovered from globals nor inferred from the Lua caller. `None` only
    // when no Rust function is running.
    pub(crate) fn active_rust_function_name(&self) -> Option<String> {
        let function = self.in_flight.active_rust_fn?;
        Some(self.rust_fns.name(function).to_owned())
    }

    /// Install the catalog or host function `function` as global `name`.
    pub(crate) fn set_global_rust_fn(&mut self, name: &str, function: RustFnId) -> Result<()> {
        self.set_global_value(name, Val::RustFn(function))
    }

    /// Creates a new Rust function entry and returns its handle; push it with
    /// [`State::push_rust_fn`]. Every call creates a new function identity,
    /// even for a function already added: values of two handles compare
    /// unequal from Lua, while every push of one handle is the same value.
    /// Entries live as long as the State, so create a handle once, during
    /// setup, rather than per call.
    ///
    /// `name` is the function's diagnostic name: argument errors raised while
    /// it runs (through [`State::check_type`], [`State::check_any`] or
    /// [`State::raw_field`]) name it, whatever the script calls it, so give
    /// the name scripts know it by (`"game.spawn"`). It is fixed here, need
    /// not be unique, and is neither saved nor part of the function's
    /// identity.
    ///
    /// The entry has no stable id, so a save that reaches it fails with
    /// `SaveError::UnregisteredFunction`; under the `snapshot` feature,
    /// `State::register_rust_fn` creates one that saves. Fails with
    /// [`ErrorKind::StringSizeExceeded`] for a name over
    /// [`crate::limits::MAX_STRING_BYTES`], and with
    /// [`ErrorKind::RustFunctionLimit`] once the State holds `u32::MAX`
    /// entries, changing nothing.
    pub fn add_rust_fn(&mut self, name: &str, function: RustFunc) -> Result<RustFnHandle> {
        self.check_usable()?;
        check_string_size(name.len())?;
        self.rust_fns.add(name, function).ok_or_else(|| {
            Error::without_location(ErrorKind::RustFunctionLimit {
                limit: RustFnTable::CAPACITY,
            })
        })
    }

    /// Register a Rust function under a stable id and install it as global
    /// `name`, returning its handle (see [`State::register_rust_fn`]). The
    /// global name is also its diagnostic name.
    ///
    /// The id must be new to this State. To install one registered function
    /// under a second global name, push its handle and call
    /// [`State::set_global`].
    ///
    /// Refused inside [`State::with_restricted_env`] with
    /// [`ErrorKind::SetGlobalNamedRustFnInRestrictedEnv`]. Its global would be
    /// discarded with the temporary environment, but its function-table
    /// entry would outlive that environment.
    ///
    /// A rejected call changes nothing: the name is checked as
    /// [`State::set_global`] checks it ([`ErrorKind::StringSizeExceeded`])
    /// before the id is registered, and the install after registration
    /// cannot fail. Restricted-environment rejection also happens before the
    /// id is registered, so the same id can be retried after the environment
    /// is restored.
    #[cfg(feature = "snapshot")]
    pub fn set_global_named_rust_fn(
        &mut self,
        name: &str,
        id: &str,
        func: RustFunc,
    ) -> Result<RustFnHandle> {
        self.check_usable()?;
        if self.in_restricted_env() {
            return Err(self.error(ErrorKind::SetGlobalNamedRustFnInRestrictedEnv));
        }
        check_string_size(name.len())?;
        let handle = self.register_rust_fn(id, name, func)?;
        let function = self
            .rust_fns
            .resolve(handle)
            .expect("this State issued the handle it just registered");
        // The length is checked, and a Rust function needs no rooting while
        // interning collects.
        let key = self.allocator().alloc_string(name.as_bytes());
        self.global_env
            .write_key(&mut self.heap, key, Val::RustFn(function));
        Ok(handle)
    }

    /// Register a Rust function under a stable id without installing it
    /// anywhere, and return its handle; push it with [`State::push_rust_fn`].
    ///
    /// Save and load resolve the function by `id`: a save writes the id of
    /// every registered function it reaches, and a load resolves it through
    /// the registrations its setup made. Ids should be stable strings owned by
    /// the embedder. Like [`State::add_rust_fn`], registration creates a new
    /// function identity: registering one Rust function under two ids gives
    /// two functions that compare unequal from Lua and save under their own
    /// ids.
    ///
    /// `name` is the diagnostic name, as for [`State::add_rust_fn`]. It is
    /// independent of `id`: the id identifies the function across saves and
    /// should survive renaming the API, while the name is what argument
    /// errors show. A load takes names from the registrations its setup
    /// makes, so a newer build can rename a function without breaking saves.
    ///
    /// The id must be new to this State, including the standard library's ids
    /// once `open_libs` ran: an id that is already registered fails with
    /// [`ErrorKind::DuplicateFunctionRegistration`], whatever function it
    /// names. Ids exceeding [`crate::limits::MAX_SAVE_IDENTIFIER_BYTES`] fail
    /// with [`ErrorKind::FunctionIdSizeExceeded`], names over
    /// [`crate::limits::MAX_STRING_BYTES`] with
    /// [`ErrorKind::StringSizeExceeded`], and a full function table with
    /// [`ErrorKind::RustFunctionLimit`]. A rejected registration changes
    /// nothing.
    #[cfg(feature = "snapshot")]
    pub fn register_rust_fn(
        &mut self,
        id: &str,
        name: &str,
        func: RustFunc,
    ) -> Result<RustFnHandle> {
        self.check_usable()?;
        if id.len() > crate::limits::MAX_SAVE_IDENTIFIER_BYTES {
            return Err(Error::without_location(ErrorKind::FunctionIdSizeExceeded {
                size: id.len(),
                limit: crate::limits::MAX_SAVE_IDENTIFIER_BYTES,
            }));
        }
        check_string_size(name.len())?;
        // An id names one function entry, so a second registration of an id
        // is refused without comparing function pointers: one Rust function
        // need not have one address.
        if self.rust_fns.is_bound(id) {
            return Err(Error::without_location(
                ErrorKind::DuplicateFunctionRegistration { id: id.to_string() },
            ));
        }
        self.rust_fns.register(id, name, func).ok_or_else(|| {
            Error::without_location(ErrorKind::RustFunctionLimit {
                limit: RustFnTable::CAPACITY,
            })
        })
    }

    /// The handle of the function registered under `id`, if any: a host
    /// entry from [`State::register_rust_fn`], or a standard-library function
    /// once `open_libs` ran (`"print"`, `"math.sin"`, ...).
    #[cfg(feature = "snapshot")]
    pub fn registered_rust_fn(&self, id: &str) -> Option<RustFnHandle> {
        self.rust_fns
            .lookup(id)
            .map(|function| self.rust_fns.handle_of(function))
    }

    /// Store global `name`. Interning the name may collect, so a heap `val`
    /// must be rooted by the caller.
    pub(super) fn set_global_value(&mut self, name: &str, val: Val) -> Result<()> {
        if let Some(key) = self.global_name_key(name, val)? {
            self.global_env.write_key(&mut self.heap, key, val);
        }
        Ok(())
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
        if let Err(e) = self.set_table_str_key_value(-1, MetaName::Index.as_str(), lib) {
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
    /// The global table keeps its identity: inside `f` it holds only the
    /// whitelisted globals, so `_G` (when whitelisted) and every reference to
    /// the table taken before the call see the restricted environment, and
    /// whatever `f` stores in it, raw keys included, is discarded with it.
    /// [`State::open_libs`] is refused inside `f` with
    /// [`ErrorKind::OpenLibsInRestrictedEnv`].
    /// `State::set_global_named_rust_fn` is refused with
    /// [`ErrorKind::SetGlobalNamedRustFnInRestrictedEnv`] because its function
    /// registration would outlive the discarded global.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::StatePoisoned`] without running `f` if the State
    /// is already poisoned, or after `f` returns if it poisoned the State
    /// (for example by catching a panic from a nested `call`). A panic out of
    /// `f` poisons the State and is resumed; the environment is still
    /// restored on the way out, but that does not make the State reusable.
    pub fn with_restricted_env<F, R>(&mut self, whitelist: &[&str], f: F) -> Result<R>
    where
        F: FnOnce(&mut Self) -> R,
    {
        self.guarded(|state| {
            let saved = state.global_env.restrict(&mut state.heap, whitelist);
            state.suspended_envs.push(saved);

            // Restore the environment on unwind too. This keeps the global
            // bindings coherent for whatever the host still reads before it
            // drops the State; it is not a reuse guarantee, since the State
            // is poisoned by the panic either way.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(state)));

            let saved = state
                .suspended_envs
                .pop()
                .expect("suspended environment missing during restoration");
            state.global_env.resume(&mut state.heap, saved);

            match result {
                Ok(r) => Ok(r),
                Err(payload) => {
                    state.poisoned = true;
                    std::panic::resume_unwind(payload)
                }
            }
        })
    }

    /// Whether a [`State::with_restricted_env`] callback is running, at any
    /// nesting depth.
    pub(super) fn in_restricted_env(&self) -> bool {
        !self.suspended_envs.is_empty()
    }

    /// The prologue of every public fallible method: refuse a poisoned State,
    /// then an unsupported floating-point environment on the calling thread
    /// (`fp_env`). One predictable branch and one control-register read; the
    /// error construction is out of line. Poison takes precedence: it is
    /// permanent, the environment is the host's to fix.
    ///
    /// The standard library calls public methods too, so it re-checks inside
    /// a call. That is redundant, since only host code changes the
    /// environment, but harmless: the read changes nothing.
    #[inline(always)]
    pub(crate) fn check_usable(&self) -> Result<()> {
        if self.poisoned || (self.host_code_active() && crate::fp_env::unsupported()) {
            return Err(self.unusable_error());
        }
        Ok(())
    }

    /// Whether the code calling into the State is host code: no Rust
    /// function is running (a direct host call), or a host function is.
    /// While a standard-library function runs, the environment was checked
    /// after the last host code that ran returned, at an entry point or at
    /// a callback's return in `invoke_rust_fn`, so its own calls skip the
    /// register read.
    #[inline(always)]
    fn host_code_active(&self) -> bool {
        !self
            .in_flight
            .active_rust_fn
            .is_some_and(rust_fn::RustFnId::is_library)
    }

    /// The refusal [`State::check_usable`] reports, poison first.
    #[cold]
    #[inline(never)]
    fn unusable_error(&self) -> Error {
        if self.poisoned {
            Self::poisoned_error()
        } else {
            Self::float_environment_error()
        }
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn poisoned_error() -> Error {
        Error::without_location(ErrorKind::StatePoisoned)
    }

    /// The refusal of the calling thread's unsupported floating-point
    /// environment, read again here. Called only after
    /// `fp_env::unsupported` said so; no host code runs in between, so the
    /// register still holds what it held then.
    #[cold]
    #[inline(never)]
    pub(crate) fn float_environment_error() -> Error {
        let environment = crate::fp_env::fault()
            .expect("called only once fp_env::unsupported found the environment unsupported");
        Error::without_location(ErrorKind::UnsupportedFloatEnvironment(environment))
    }

    /// [`State::check_usable`] for the snapshot entry points, which report
    /// through [`SaveError`].
    #[cfg(feature = "snapshot")]
    #[inline(always)]
    pub(crate) fn check_save_usable(&self) -> std::result::Result<(), SaveError> {
        if self.poisoned {
            return Err(SaveError::StatePoisoned);
        }
        if let Some(environment) = crate::fp_env::fault() {
            return Err(SaveError::UnsupportedFloatEnvironment(environment));
        }
        Ok(())
    }

    /// Whether a panic has unwound through this State's execution. A
    /// poisoned State refuses its fallible operations with
    /// [`ErrorKind::StatePoisoned`] (snapshot saving with its own
    /// state-poisoned error). Infallible host controls and observations,
    /// such as cost observations, user data, counters and this method, remain
    /// available. Fallible operations such as `set_cost_budget` are refused.
    /// Dropping the State is always safe.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// The boundary for public entry points that execute Lua, metamethods
    /// or host callbacks.
    ///
    /// Refuses a poisoned State. Any unwind out of `f` poisons the State
    /// before anything else happens. The internal [`PoisonUnwind`] marker
    /// (raised by `invoke_rust_fn` when a callback swallowed a panic) is
    /// translated into `Err(StatePoisoned)` here; any other payload is
    /// resumed unchanged so the host sees its own panic. A normal return
    /// from `f` on a State that became poisoned meanwhile is also refused.
    ///
    /// Translating the marker into an error is only correct because this
    /// guard returns straight to host code: no interrupted VM frame sits
    /// between it and its caller whose ordinary error cleanup would then run
    /// against abandoned bookkeeping. VM-internal callers must therefore use
    /// the unguarded bodies (`call_unguarded` and kin), never the public
    /// guarded entry points. A RustFn that calls a guarded entry gets the
    /// error back, and `invoke_rust_fn` re-raises the marker when that
    /// callback returns.
    ///
    /// Which public methods need this boundary is derived, not chosen:
    /// `scripts/poison_contract.py` requires the whole body of every public
    /// method that can run Lua (the native-call closure) or runs host code
    /// against the State (its signature takes a callable over `&mut State`)
    /// to be `self.guarded(|state| ...)`, allows the guard nowhere else, and
    /// allows only the CLI to call a guarded method. The split is not a
    /// type on purpose: a capability token minted here and in
    /// `invoke_rust_fn` would have to reach the dispatch loop and every
    /// library callback through the public `RustFunc` signature, and with a
    /// crate-visible constructor it would constrain nothing inside the
    /// crate, where every caller of the unguarded bodies lives. Nor would it
    /// stop VM code from calling a guarded entry, the converse mistake.
    pub(crate) fn guarded<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
        self.check_usable()?;
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self))) {
            Ok(result) => {
                if self.poisoned {
                    drop(result);
                    return Err(Self::poisoned_error());
                }
                #[cfg(debug_assertions)]
                if self.in_flight.active_rust_fn.is_none() {
                    self.debug_assert_at_rest();
                }
                result
            }
            Err(payload) => {
                // Poison unconditionally: even a marker payload may come from
                // a different State whose panic a host resumed inside this
                // one, and it abandoned this State's frames all the same.
                self.poisoned = true;
                if payload.is::<PoisonUnwind>() {
                    Err(Self::poisoned_error())
                } else {
                    std::panic::resume_unwind(payload)
                }
            }
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
        // allocate, call Lua, or re-enter the host. The unwind guard keeps
        // this helper's own root bookkeeping balanced and poisons the State;
        // it does not make the State reusable after a panic.
        let watermark = self.in_flight.rooted_values.len();
        self.in_flight.rooted_values.extend_from_slice(values);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        self.in_flight.rooted_values.truncate(watermark);
        match result {
            Ok(result) => result,
            Err(payload) => {
                self.poisoned = true;
                std::panic::resume_unwind(payload)
            }
        }
    }

    /// Allocates a string on the heap.
    #[cfg_attr(feature = "hotpath", hotpath::measure)]
    pub(super) fn alloc_string(&mut self, bytes: impl AsRef<[u8]>) -> Result<Val> {
        check_string_size(bytes.as_ref().len())?;
        let ptr = self.allocator().alloc_string(bytes.as_ref());
        Ok(Val::Str(ptr))
    }

    /// Construct an [`Error`] of the given kind with no source position.
    ///
    /// The position is filled in later from the innermost Lua activation,
    /// when the error is first reported or leaves that frame (see
    /// `locate_error`), because that is the first point with access to the
    /// frame's line information. Errors with no enclosing Lua activation -
    /// those raised by a host `RustFunc` called directly - keep a zero
    /// position.
    pub fn error(&self, kind: ErrorKind) -> Error {
        // Retain this public compatibility helper. Frame exit supplies the
        // location; locating here would confuse parser and caller locations.
        Error::without_location(kind)
    }

    pub(super) fn type_error(&self, e: TypeError) -> Error {
        self.error(ErrorKind::TypeError(e))
    }

    /// The source line of the instruction before `cursor` in `bytecode`, the
    /// one a frame suspended or failed at (0 if unknown). A cursor of 0 reads
    /// the first instruction's line.
    pub(super) fn line_before(bytecode: &Bytecode, cursor: usize) -> u32 {
        bytecode
            .line_info
            .get(cursor.saturating_sub(1))
            .copied()
            .unwrap_or(0)
    }

    fn trace_frame(bytecode: &Bytecode, cursor: usize, tail_called: bool) -> StackFrame {
        StackFrame {
            function_name: bytecode.name.clone(),
            source: bytecode.source.clone(),
            line: Self::line_before(bytecode, cursor),
            defined_line: bytecode.defined_line,
            tail_called,
        }
    }

    /// Build a stack trace for an error located in the innermost activation,
    /// `call_stack.last()`, whose code is `bytecode` at `cursor`: the
    /// executing frame's own cursor when it unwinds, or the record's call
    /// site when the error is returned from a call it suspended at. The
    /// callers come from `call_stack`, each at its recorded call site.
    pub(super) fn build_stack_trace(
        &self,
        bytecode: &Arc<Bytecode>,
        cursor: usize,
    ) -> Vec<StackFrame> {
        let mut trace = Vec::with_capacity(self.in_flight.call_stack.len() + 1);

        // First entry: the current frame where the error occurred
        // A host may call the iterator without a script caller. Keep its
        // implementation location in that case rather than inventing one.
        let shim_with_script_caller = Self::is_gmatch_wrapper(bytecode)
            && self
                .in_flight
                .call_stack
                .iter()
                .rev()
                .skip(1)
                .any(|info| !Self::is_gmatch_wrapper(&info.bytecode));
        if !shim_with_script_caller {
            let tail_called = self
                .in_flight
                .call_stack
                .last()
                .is_some_and(|info| info.chain.tail_called());
            trace.push(Self::trace_frame(bytecode, cursor, tail_called));
        }

        // Add entries from call_stack (most recent first).
        // Skip the last entry (current function) since we already have it.
        // The remaining entries are the callers, with ip pointing to their call sites.
        for call_info in self.in_flight.call_stack.iter().rev().skip(1) {
            if Self::is_gmatch_wrapper(&call_info.bytecode) {
                continue;
            }
            trace.push(Self::trace_frame(
                &call_info.bytecode,
                call_info.ip,
                call_info.chain.tail_called(),
            ));
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
        assert!(
            (0..Builtin::COUNT)
                .all(|slot| state.global_env.read_builtin(&state.heap, slot as u8) != Val::Nil)
        );
    }

    #[test]
    fn closures_of_one_bytecode_share_runtime_bundle() {
        let mut state = State::raw();
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
        crate::Program::compile(
            "function make() return function(t) return t.v end end \
                 r1 = make() r2 = make() warmed = r1({ v = 3 })",
            None,
        )
        .and_then(|program| state.load(&program))
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
        let mut state = State::raw();
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
        // The interned metamethod and builtin names are permanent roots on
        // top of that.
        let names = MetaName::ALL.len() + Builtin::COUNT;
        assert_eq!(state.string_count(), names + 1);
        state.gc_collect();
        assert_eq!(state.string_count(), names);
    }

    /// One populated runtime cache cell, copied whole.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum CachedEntry {
        Field(caches::FieldLookupCacheEntry),
        SetField(caches::FieldLookupCacheEntry),
        Method(caches::MethodLookupCacheEntry),
        StringMethod(caches::StringMethodCacheEntry),
        Tfor(caches::TforCursorEntry),
    }

    impl CachedEntry {
        fn family(self) -> &'static str {
            match self {
                Self::Field(_) => "field",
                Self::SetField(_) => "set-field",
                Self::Method(_) => "method",
                Self::StringMethod(_) => "string-method",
                Self::Tfor(_) => "tfor cursor",
            }
        }

        /// The handles the entry names, enumerated here independently of
        /// the collector's own enumeration.
        fn handles(self) -> Vec<Val> {
            match self {
                Self::Field(e) | Self::SetField(e) => vec![Val::Obj(e.table)],
                Self::Method(e) => vec![Val::Obj(e.receiver_metatable), Val::Obj(e.method_table)],
                Self::StringMethod(e) => vec![Val::Obj(e.string_lib)],
                Self::Tfor(e) => vec![Val::Obj(e.table)],
            }
        }
    }

    /// (runtime identity, family, slot, position within the slot's family).
    type CellKey = (usize, &'static str, usize, usize);
    type CacheInventory = (
        Vec<(CellKey, CachedEntry)>,
        std::collections::BTreeSet<&'static str>,
    );

    /// Every populated cell, keyed by [`CellKey`]. Only method entries have
    /// more than one position. The exhaustive cache bundle destructuring
    /// forces this test oracle to account for each cache family.
    fn cache_cells(state: &State) -> CacheInventory {
        let mut cells = Vec::new();
        let mut families = std::collections::BTreeSet::new();
        for (identity, entry) in &state.bytecode_caches {
            let caches::RuntimeCaches {
                // Global lookup entries hold indexes into the State's live
                // global table, so they name no object that can be collected.
                global_lookup: _,
                field_lookup,
                set_field_lookup,
                tfor_cursor,
                // The collector's mark flag, not a cache family.
                reached: _,
            } = &entry.runtime.caches;
            // Register each enumeration even if its slots are all empty.
            // The exhaustive bundle destructuring above requires new fields
            // to be assigned an enumeration here.
            let mut visit =
                |family: &'static str, entries: Vec<(usize, usize, Option<CachedEntry>)>| {
                    families.insert(family);
                    for (slot, position, cell) in entries {
                        if let Some(cell) = cell {
                            assert_eq!(family, cell.family());
                            cells.push(((*identity, cell.family(), slot, position), cell));
                        }
                    }
                };
            visit(
                "field",
                field_lookup
                    .iter()
                    .enumerate()
                    .map(|(slot, cell)| (slot, 0, cell.get_field().map(CachedEntry::Field)))
                    .collect(),
            );
            visit(
                "method",
                field_lookup
                    .iter()
                    .enumerate()
                    .flat_map(|(slot, cell)| {
                        (0..caches::METHOD_ENTRIES).map(move |position| {
                            (
                                slot,
                                position,
                                cell.get_method(position).map(CachedEntry::Method),
                            )
                        })
                    })
                    .collect(),
            );
            visit(
                "string-method",
                field_lookup
                    .iter()
                    .enumerate()
                    .map(|(slot, cell)| {
                        (
                            slot,
                            0,
                            cell.get_string_method().map(CachedEntry::StringMethod),
                        )
                    })
                    .collect(),
            );
            visit(
                "set-field",
                set_field_lookup
                    .iter()
                    .enumerate()
                    .map(|(slot, cell)| (slot, 0, cell.get().map(CachedEntry::SetField)))
                    .collect(),
            );
            visit(
                "tfor cursor",
                tfor_cursor
                    .iter()
                    .enumerate()
                    .map(|(slot, cell)| (slot, 0, cell.get().map(CachedEntry::Tfor)))
                    .collect(),
            );
        }
        (cells, families)
    }

    /// Between prunes a collection leaves an entry naming a freed object in
    /// place, and the entry only misses: the next lookup through the site
    /// answers from the live table and refills the entry. The pruning
    /// collection then clears it.
    #[test]
    fn collections_between_prunes_keep_dead_entries_that_only_miss() {
        fn field_handles(state: &State) -> Vec<Val> {
            cache_cells(state)
                .0
                .into_iter()
                .filter(|(_, cell)| cell.family() == "field")
                .flat_map(|(_, cell)| cell.handles())
                .collect()
        }
        fn run(state: &mut State, source: &str) {
            crate::Program::compile(source, None)
                .and_then(|program| state.load(&program))
                .and_then(|()| state.call(ArgCount::Fixed(0), RetCount::Fixed(0)))
                .expect("chunk runs");
        }
        let mut state = State::new();
        state.gc_disable_auto();
        // A pruning collection first, so the next one is known not to prune.
        state.heap.force_runtime_cache_prune();
        state.gc_collect();
        run(&mut state, "function get(t) return t.k end get({ k = 1 })");
        state.gc_collect();
        let stale = field_handles(&state);
        assert_eq!(stale.len(), 1, "one field site was warmed");
        assert!(
            !state.heap.val_is_live(&stale[0]),
            "a collection between prunes keeps the dead entry"
        );
        run(
            &mut state,
            "for i = 1, 8 do if get({ k = i }) ~= i then error('stale hit') end end",
        );
        let refilled = field_handles(&state);
        assert_eq!(refilled.len(), 1);
        assert_ne!(
            refilled[0], stale[0],
            "the dead entry missed and was refilled"
        );
        state.heap.force_runtime_cache_prune();
        state.gc_collect();
        assert!(field_handles(&state).is_empty(), "the prune clears it");
    }

    /// Cache handles are weak: a pruning collection that frees a cached
    /// object must clear every entry naming it, or a later object reusing its
    /// arena slot with a wrapped generation would satisfy the entry's handle
    /// check.
    /// Each family is warmed against objects that then become unreachable
    /// while the functions holding the sites stay reachable, and against
    /// objects that stay alive. Method entries name two handles, so each is
    /// also exercised dying alone: a method table replaced under a surviving
    /// metatable, and a dead metatable whose method table survives. Method
    /// entries cache hits only, so a miss and function or string handlers
    /// leave none.
    #[test]
    fn collection_clears_cache_entries_naming_freed_objects() {
        let mut state = State::new();
        state.gc_disable_auto();
        crate::Program::compile(
            "function get(t) return t.k end \
             function set(t) t.k = 2 end \
             function method(t) return t.m end \
             function smethod(s) return s.up end \
             function iter(t) for _ in pairs(t) do end end \
             function thandler(t) return t.m end \
             function tabsent(t) return t.z end \
             function fhandler(t) return t.z end \
             function shandler(t) return t.z end \
             function mtdead(t) return t.m end \
             local n = 1 \
             get({ k = 1 }) set({ k = 1 }) \
             method(setmetatable({}, { __index = { m = 1 } })) \
             iter({ a = 1 }) \
             local smt = getmetatable('') \
             local lib = smt.__index \
             smt.__index = { up = string.upper } \
             smethod('x') \
             smt.__index = lib \
             mt_t = { __index = { m = 1 } } \
             mt_f = { __index = function() return 1 end } \
             mt_s = { __index = 'h' .. n } \
             mt_a = { __index = {} } \
             thandler(setmetatable({}, mt_t)) \
             tabsent(setmetatable({}, mt_a)) \
             fhandler(setmetatable({}, mt_f)) \
             shandler(setmetatable({}, mt_s)) \
             mt_t.__index = nil mt_a.__index = nil mt_f.__index = nil mt_s.__index = nil \
             keep_handler = { m = 1 } \
             mtdead(setmetatable({}, { __index = keep_handler })) \
             function get_live(t) return t.k end \
             function set_live(t) t.k = 2 end \
             function method_live(t) return t.m end \
             function iter_live(t) for _ in pairs(t) do end end \
             function smethod_live(s) return s.upper end \
             keep = setmetatable({ k = 1 }, { __index = { m = 1 } }) \
             get_live(keep) set_live(keep) method_live(keep) iter_live(keep) \
             smethod_live('x') \
             function dead_first(t) return t.m end \
             function dead_second(t) return t.m end \
             dead_first(setmetatable({}, { __index = { m = 1 } })) dead_first(keep) \
             dead_second(keep) dead_second(setmetatable({}, { __index = { m = 1 } }))",
            None,
        )
        .and_then(|program| state.load(&program))
        .expect("cache warmup source compiles");
        // A second copy keeps the chunk itself rooted, so every runtime
        // survives the collection and only entries can be dropped.
        state.push_value(-1).expect("chunk copy fits");
        state
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("cache warmup runs");

        let (before, families) = cache_cells(&state);
        state.heap.force_runtime_cache_prune();
        state.gc_collect();
        let (after, _) = cache_cells(&state);

        let live = |val: &Val| state.heap.val_is_live(val);
        // Fixed, not derived from `before`: a family whose warmup stopped
        // populating any cell must fail here rather than drop out unchecked.
        let required = [
            "field",
            "set-field",
            "method",
            "string-method",
            "tfor cursor",
        ];
        assert_eq!(
            families,
            required
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
        for family in required {
            let cells = before.iter().filter(|((_, f, _, _), _)| *f == family);
            type Cells<'a> = Vec<&'a (CellKey, CachedEntry)>;
            let (dead, alive): (Cells<'_>, Cells<'_>) =
                cells.partition(|(_, cell)| !cell.handles().iter().all(live));
            assert!(
                !dead.is_empty(),
                "{family}: no warmed entry named a freed object"
            );
            assert!(
                !alive.is_empty(),
                "{family}: no warmed entry named only survivors"
            );
        }
        assert!(
            before
                .iter()
                .any(|(_, cell)| matches!(cell, CachedEntry::Method(e)
                if live(&Val::Obj(e.receiver_metatable)) && !live(&Val::Obj(e.method_table)))),
            "no method entry had its method table freed under a surviving metatable"
        );
        assert!(
            before
                .iter()
                .any(|(_, cell)| matches!(cell, CachedEntry::Method(e)
                if !live(&Val::Obj(e.receiver_metatable)) && live(&Val::Obj(e.method_table)))),
            "no method entry had a freed metatable with a surviving method table"
        );
        let method_entries = before
            .iter()
            .filter(|(_, cell)| matches!(cell, CachedEntry::Method(_)))
            .count();
        // method, thandler, mtdead and method_live, plus both positions of
        // dead_first and dead_second; the miss behind tabsent and the
        // function and string handlers behind fhandler and shandler are
        // never cached.
        assert_eq!(method_entries, 8, "only proven hits are cached");
        // One site holds a dying entry beside a surviving one in each
        // position order, so clearing must be per position and must not
        // move the survivor.
        for (dead_position, live_position) in [(0, 1), (1, 0)] {
            let mixed = before.iter().any(|((id, family, slot, position), cell)| {
                *family == "method"
                    && *position == dead_position
                    && !cell.handles().iter().all(live)
                    && before.iter().any(|((id2, f2, slot2, p2), other)| {
                        id2 == id
                            && f2 == family
                            && slot2 == slot
                            && *p2 == live_position
                            && other.handles().iter().all(live)
                    })
            });
            assert!(
                mixed,
                "no method site had a freed entry at {dead_position} beside a live one"
            );
        }

        for (key, cell) in &before {
            let kept = after.iter().find(|(k, _)| k == key).map(|(_, c)| c);
            if cell.handles().iter().all(live) {
                assert_eq!(
                    kept,
                    Some(cell),
                    "{key:?}: a live entry changed or was dropped"
                );
            } else {
                assert_eq!(kept, None, "{key:?}: an entry still names a freed object");
            }
        }
        assert_eq!(
            after.len(),
            before
                .iter()
                .filter(|(_, c)| c.handles().iter().all(live))
                .count(),
            "collection must not create entries"
        );
        // The same invariant through the collector's own enumeration, which
        // covers every family and handle field.
        for entry in state.bytecode_caches.values() {
            for val in entry.runtime.caches.named_handles() {
                assert!(live(&val), "a cache entry names a freed value");
            }
        }
    }

    /// The receiver metatables a site's method entries name, by position.
    fn method_positions(state: &State) -> Vec<[Option<ObjectPtr>; caches::METHOD_ENTRIES]> {
        let mut sites = Vec::new();
        for entry in state.bytecode_caches.values() {
            for slot in &entry.runtime.caches.field_lookup {
                let positions = std::array::from_fn(|position| {
                    slot.get_method(position).map(|e| e.receiver_metatable)
                });
                if positions.iter().any(Option::is_some) {
                    sites.push(positions);
                }
            }
        }
        sites
    }

    fn global_ptr(state: &mut State, name: &str) -> ObjectPtr {
        state.get_global(name).expect("global fits");
        state
            .pop_val()
            .as_object_ptr()
            .expect("the global is an object")
    }

    /// The two method entries of one site replace the one not used last,
    /// refill a class in place instead of naming it twice, and take a
    /// position a collection emptied before evicting a survivor.
    #[test]
    fn method_entries_replace_least_recently_used_and_never_duplicate() {
        let mut state = State::new();
        state.gc_disable_auto();
        let run = |state: &mut State, source: &str| {
            crate::Program::compile(source, None)
                .and_then(|program| state.load(&program))
                .expect("test source compiles");
            state
                .call(ArgCount::Fixed(0), RetCount::Fixed(0))
                .expect("test source runs");
        };
        run(
            &mut state,
            "function site(t) return t.m end \
             mta = { __index = { m = 1 } } mtb = { __index = { m = 1 } } \
             mtc = { __index = { m = 1 } } \
             a = setmetatable({}, mta) b = setmetatable({}, mtb) \
             c = setmetatable({}, mtc) \
             site(a) site(b) site(a) site(c)",
        );
        let (mta, mtb, mtc) = (
            global_ptr(&mut state, "mta"),
            global_ptr(&mut state, "mtb"),
            global_ptr(&mut state, "mtc"),
        );
        // b was used less recently than a, so c replaced it.
        assert_eq!(method_positions(&state), vec![[Some(mta), Some(mtc)]]);

        // a was used last, so the victim is c's position. a's handler table
        // then changes and its entry no longer validates; the refill
        // rewrites a's own position rather than evicting c.
        run(&mut state, "site(a) mta.__index = { m = 2 } site(a)");
        assert_eq!(method_positions(&state), vec![[Some(mta), Some(mtc)]]);

        // c is used last, so the victim is a's position; then c dies. Its
        // position empties on a pruning collection and the next class takes
        // it instead of a's. (Between prunes the dead entry would hold its
        // position, so that fill would evict a instead: one more miss for a,
        // never a different answer.)
        run(&mut state, "site(c) c = nil mtc = nil");
        state.heap.force_runtime_cache_prune();
        state.gc_collect();
        assert_eq!(method_positions(&state), vec![[Some(mta), None]]);
        run(&mut state, "site(b)");
        assert_eq!(method_positions(&state), vec![[Some(mta), Some(mtb)]]);
    }

    #[test]
    fn live_parent_retains_nested_runtime_entry() {
        let mut state = State::raw();
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

#[cfg(all(test, feature = "snapshot"))]
mod snapshot_env_alias_tests {
    use super::{Builtin, DefaultCallbacks, State, Val};
    use crate::instr::{ArgCount, RetCount};
    use std::convert::Infallible;

    #[test]
    fn aliased_builtin_slots_use_last_environment_token_on_load() {
        let mut state = State::new();
        state.get_global("math").expect("math builtin exists");
        state
            .set_global("string")
            .expect("string builtin can be rebound");

        let Val::Obj(shared_table) = state
            .global_env
            .read_builtin(&state.heap, Builtin::Math as u8)
        else {
            panic!("math builtin is a table");
        };
        assert_eq!(
            state
                .global_env
                .read_builtin(&state.heap, Builtin::String as u8),
            Val::Obj(shared_table)
        );

        state.capture_env_tables();
        assert_eq!(
            super::save_state::environment_tokens(&state)
                .get(&shared_table)
                .copied(),
            Some("string")
        );

        let save = state.save_state().expect("aliased environment saves");
        let mut loaded = State::load_state(&save.bytes, Box::new(DefaultCallbacks), |_| {
            Ok::<(), Infallible>(())
        })
        .expect("aliased environment loads");
        crate::Program::compile(
            "if math ~= string then error('builtin alias changed') end; \
                 if math.match == nil then error('last token did not resolve') end",
            None,
        )
        .and_then(|program| loaded.load(&program))
        .expect("alias check compiles");
        loaded
            .call(ArgCount::Fixed(0), RetCount::Fixed(0))
            .expect("alias check runs");
    }
}

#[cfg(test)]
mod tests;
