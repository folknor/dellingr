//! The inventories of `State`'s fields, each exhaustive by construction.
//!
//! - [`State::for_each_gc_root`] answers "what does this field root?". It is
//!   the single source of truth for the collector's root set (heap objects,
//!   upvalue-pool slots and bytecode-cache retention alike) and, through
//!   [`GcRoot`], for what a snapshot does with each root.
//! - [`State::rest_state`] answers "how long does this field's content
//!   live?": execution-lifetime ([`InFlight`]), host-held (allowed outside
//!   execution, refused by a snapshot) or persistent.
//! - [`State::for_each_gc_cleanup`] answers "what post-sweep cleanup does this
//!   field need?": weak tables are pruned and the upvalue pool is swept.
//!
//! Each destructures `State` (and the root inventory also destructures
//! `InFlight`) without a rest pattern, so a new field fails to compile until
//! it is named in each, either used or bound to `_` with the reason it needs
//! nothing there. That forces a decision; it cannot prove the decision right.

use std::sync::Arc;

use super::anchor::Registry;
use super::global_env::{GlobalEnv, SuspendedEnv};
use super::meta_name::MetaNames;
use super::object::{GcHeap, ObjectPtr, StringPtr, UpvaluePool, UpvalueRef};
use super::{CallInfo, IndexMap, PatternCacheEntry, PointerIdKey, RustFunc, State, Val};
use crate::compiler::Bytecode;
use crate::limits::tuning;
use crate::patterns::AnchorMode;

/// Execution-lifetime state: what a call in progress holds, and nothing that
/// outlives it. Every field is at its [`InFlight::new`] value once the
/// outermost host entry point returns, and must be for a snapshot.
///
/// A field belongs here exactly when that holds for it; [`State::rest_state`]
/// makes every other State field say why it does not.
pub(crate) struct InFlight {
    /// The bottom index of the current frame in the stack.
    pub(crate) stack_bottom: usize,
    /// One record per active Lua activation, innermost last; tracebacks,
    /// `print` locations and the bytecode-cache sweep read it.
    pub(crate) call_stack: Vec<CallInfo>,
    /// Extra arguments of active Lua functions, in activation order. Each
    /// record owns the suffix from `CallInfo::vararg_start` while innermost.
    /// Only the live length is a GC root; retained capacity is not saved.
    pub(crate) vararg_stack: Vec<Val>,
    /// Open upvalues currently pointing to stack slots.
    /// Each entry is (stack_index, upvalue_ref). Kept sorted by stack_index ascending
    /// so we can efficiently close them when a function returns.
    pub(crate) open_upvalues: Vec<(usize, UpvalueRef)>,
    /// Identities being turned into closure shells, retained during a GC sweep.
    pub(crate) pending_bytecode_caches: Vec<usize>,
    /// Temporary VM values retained across allocation or re-entry (see
    /// `State::with_rooted_values`).
    pub(crate) rooted_values: Vec<Val>,
    /// Depth of __index, __newindex and non-function __call chains.
    /// Prevents stack overflow from circular metamethod references.
    pub(crate) metamethod_depth: u32,
    /// Active Lua activations (the length of `call_stack`, kept as a counter
    /// for the hot depth check). Lua-to-Lua calls run in one dispatch loop
    /// and cost no native stack, so this bounds heap-held frames only.
    pub(crate) call_depth: u32,
    /// Active calls made from Rust: host `call`/`call_anchor`, metamethod
    /// handlers, generic-for iterators and library callbacks, each of which
    /// starts a native dispatch-loop invocation or Rust callback. Bounded by
    /// `MAX_NATIVE_CALL_DEPTH`; see docs/limits.md, "Native stack".
    pub(crate) native_call_depth: u32,
    /// Active native callback, restored across nested Rust calls.
    pub(crate) active_rust_fn: Option<RustFunc>,
}

impl InFlight {
    pub(super) fn new() -> Self {
        Self {
            stack_bottom: 0,
            call_stack: Vec::with_capacity(tuning::INITIAL_CALL_STACK_CAPACITY),
            vararg_stack: Vec::new(),
            open_upvalues: Vec::new(),
            pending_bytecode_caches: Vec::new(),
            rooted_values: Vec::with_capacity(tuning::INITIAL_TRANSIENT_ROOT_VALUES_CAPACITY),
            metamethod_depth: 0,
            call_depth: 0,
            native_call_depth: 0,
            active_rust_fn: None,
        }
    }

    /// Whether every field is at its [`InFlight::new`] value. Presence, not
    /// equality: `active_rust_fn` is a function pointer, and comparing those
    /// is unreliable.
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    pub(super) fn is_at_rest(&self) -> bool {
        let Self {
            stack_bottom,
            call_stack,
            vararg_stack,
            open_upvalues,
            pending_bytecode_caches,
            rooted_values,
            metamethod_depth,
            call_depth,
            native_call_depth,
            active_rust_fn,
        } = self;
        *stack_bottom == 0
            && call_stack.is_empty()
            && vararg_stack.is_empty()
            && open_upvalues.is_empty()
            && pending_bytecode_caches.is_empty()
            && rooted_values.is_empty()
            && *metamethod_depth == 0
            && *call_depth == 0
            && *native_call_depth == 0
            && active_rust_fn.is_none()
    }

    /// Return every field to its [`InFlight::new`] value, keeping capacity.
    #[cfg(feature = "snapshot")]
    pub(super) fn clear(&mut self) {
        let Self {
            stack_bottom,
            call_stack,
            vararg_stack,
            open_upvalues,
            pending_bytecode_caches,
            rooted_values,
            metamethod_depth,
            call_depth,
            native_call_depth,
            active_rust_fn,
        } = self;
        *stack_bottom = 0;
        call_stack.clear();
        vararg_stack.clear();
        open_upvalues.clear();
        pending_bytecode_caches.clear();
        rooted_values.clear();
        *metamethod_depth = 0;
        *call_depth = 0;
        *native_call_depth = 0;
        *active_rust_fn = None;
    }
}

/// Where a State stands with respect to execution, from [`State::rest_state`].
#[cfg(any(debug_assertions, feature = "snapshot"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestState {
    /// A call is in progress: some [`InFlight`] field is set, or a pooled
    /// frame stack still holds frames.
    Executing,
    /// No call is in progress, but the host holds values on the stack or is
    /// inside `with_restricted_env`. Legal between host calls; refused by a
    /// snapshot.
    HostHeld,
    /// Nothing but persistent state: a snapshot may be taken.
    Quiescent,
}

/// One State-owned root source. The collector marks each kind
/// (`State::mark_roots`); a snapshot accounts for each kind
/// (`save_state::save_coverage`). Both matches are exhaustive, so a new kind
/// needs a decision in each. Graph edges (table contents, closure literals
/// and upvalues) belong to the objects that hold them, not here.
#[derive(Clone, Copy)]
pub(super) enum GcRoot<'a> {
    /// The value stack: host-pushed values and every live Lua register,
    /// executing closures included (each sits in its function slot).
    Stack(&'a [Val]),
    /// The global table, its builtin names and mirror.
    Globals(&'a GlobalEnv),
    /// The interned literals of one runtime-cache entry.
    ChunkLiterals(&'a [Val]),
    /// Metamethod event names, interned for the State's life.
    MetaNames(&'a MetaNames),
    /// The metatable shared by every string. Strings carry no metatable
    /// pointer, so nothing else roots it.
    StringMetatable(ObjectPtr),
    /// Values the host retains through `Anchor` handles.
    Anchors(&'a Registry),
    /// Values temporarily held outside the visible stack.
    RootedValues(&'a [Val]),
    /// One global environment suspended by `with_restricted_env`.
    SuspendedEnv(&'a SuspendedEnv),
    /// Extra arguments of active Lua functions (the live length only).
    Varargs(&'a [Val]),
    /// An open upvalue's pool slot: it may not yet be held by any closure
    /// (mid-capture in `reg_closure`), so it is a root for the pool sweep
    /// even though it holds no heap reference.
    OpenUpvalue(UpvalueRef),
    /// The bytecode of an active activation: its runtime-cache entry, and
    /// those of its nested prototypes, must survive the sweep.
    ActiveChunk(&'a Arc<Bytecode>),
    /// A runtime-cache identity being turned into a closure shell.
    PendingChunk(usize),
    /// A canonical environment object captured by `open_libs`, kept even
    /// when user code reaches none of it: save/load diffs against it.
    #[cfg(feature = "snapshot")]
    EnvObject(ObjectPtr),
    /// The pristine contents captured with an environment object.
    #[cfg(feature = "snapshot")]
    EnvBaseline(&'a super::EnvBaseline),
}

impl GcRoot<'_> {
    /// Whether this root holds nothing. Only a slice can be empty; every
    /// other kind is visited once per thing it holds.
    #[cfg(feature = "snapshot")]
    pub(super) fn is_empty(&self) -> bool {
        match self {
            GcRoot::Stack(values)
            | GcRoot::ChunkLiterals(values)
            | GcRoot::RootedValues(values)
            | GcRoot::Varargs(values) => values.is_empty(),
            _ => false,
        }
    }
}

/// One State-owned weak table that must be pruned after the heap sweep.
///
/// This closed enum makes adding a weak table require adding its pruning
/// behavior to [`WeakTable::prune`].
pub(super) enum WeakTable<'a> {
    /// Compiled string patterns keyed by interned strings.
    Patterns {
        entries: &'a mut IndexMap<(StringPtr, AnchorMode), PatternCacheEntry>,
        byte_count: &'a mut usize,
    },
    /// Stable `%p` identities keyed by heap handles.
    FormatPointerIds(&'a mut IndexMap<PointerIdKey, u64>),
}

impl WeakTable<'_> {
    /// Drop entries whose referents were freed by the latest heap sweep.
    pub(super) fn prune(self, heap: &GcHeap) {
        match self {
            WeakTable::Patterns {
                entries,
                byte_count,
            } => {
                // Pattern keys are weak. Prune before any allocation can reuse
                // a swept string slot; retain preserves insertion order, so
                // the oldest surviving entry remains the next eviction victim.
                let mut pattern_bytes = 0;
                entries.retain(|(ptr, _), entry| {
                    let live = heap.val_is_live(&Val::Str(*ptr));
                    if live {
                        pattern_bytes += entry.byte_len;
                    }
                    live
                });
                *byte_count = pattern_bytes;
            }
            WeakTable::FormatPointerIds(entries) => {
                // These entries are deliberately not GC roots. An entry whose
                // value no longer resolves cannot be observed again, so drop
                // it before a later allocation could reuse its key.
                entries.retain(|key, _| heap.val_is_live(&key.as_val()));
            }
        }
    }
}

/// An ordered piece of post-sweep GC cleanup.
pub(super) enum GcCleanup<'a> {
    /// A State-owned weak table.
    WeakTable(WeakTable<'a>, &'a GcHeap),
    /// Recycle unmarked upvalue-pool slots.
    UpvaluePool(&'a mut UpvaluePool),
}

impl State {
    /// Visit every post-sweep cleanup step in collection order. A new State
    /// field fails to compile here until it is classified. Weak table variants
    /// are closed and their pruning is exhaustive in [`WeakTable::prune`].
    pub(super) fn for_each_gc_cleanup(&mut self, mut visit: impl for<'a> FnMut(GcCleanup<'a>)) {
        // A field bound to `_` holds strong references or no heap handles,
        // so a sweep leaves nothing in it to prune; `bytecode_caches` is the
        // exception, whose weak cache cells `drop_dead_cache_entries` clears
        // just before this runs.
        let State {
            #[cfg(feature = "fault-injection")]
                fault_injection: _,
            global_env: _,
            string_metatable: _,
            string_mt_index_shape: _,
            meta_names: _,
            stack: _,
            in_flight: _,
            heap,
            bytecode_caches: _,
            suspended_envs: _,
            upvalue_pool,
            cost: _,
            patterns,
            pattern_bytes,
            #[cfg(test)]
                pattern_compilations: _,
            poisoned: _,
            frame_stacks: _,
            callbacks: _,
            user_data: _,
            rng: _,
            registry: _,
            format_pointer_ids,
            next_format_pointer_id: _,
            #[cfg(feature = "snapshot")]
                rust_fns_by_id: _,
            #[cfg(feature = "snapshot")]
                rust_fn_ids_by_addr: _,
            #[cfg(feature = "snapshot")]
                env_tokens: _,
            #[cfg(feature = "snapshot")]
                env_baselines: _,
        } = self;

        visit(GcCleanup::WeakTable(
            WeakTable::Patterns {
                entries: patterns,
                byte_count: pattern_bytes,
            },
            heap,
        ));
        visit(GcCleanup::UpvaluePool(upvalue_pool));
        visit(GcCleanup::WeakTable(
            WeakTable::FormatPointerIds(format_pointer_ids),
            heap,
        ));
    }

    /// Visit every State-owned GC root. A new State or [`InFlight`] field
    /// fails to compile here until it is either visited or bound to `_` with
    /// the reason it roots nothing.
    pub(super) fn for_each_gc_root<'a>(&'a self, mut visit: impl FnMut(GcRoot<'a>)) {
        let State {
            #[cfg(feature = "fault-injection")]
                fault_injection: _, // Countdown and event log: no values.
            global_env,
            string_metatable,
            string_mt_index_shape: _, // A (shape version, slot) pair: no handle.
            meta_names,
            stack,
            heap: _, // The graph itself: roots point into it.
            bytecode_caches,
            suspended_envs,
            upvalue_pool: _, // Slots are marked through closures and OpenUpvalue.
            cost: _,         // Counters only.
            patterns: _,     // Weak: pruned by for_each_gc_cleanup after sweep.
            pattern_bytes: _,
            #[cfg(test)]
                pattern_compilations: _,
            poisoned: _,
            frame_stacks: _, // Frames own only Arcs; their chunks are in call_stack.
            callbacks: _,    // Host-owned; the host cannot hold a Val.
            user_data: _,    // Host-owned; the host cannot hold a Val.
            rng: _,
            registry,
            format_pointer_ids: _, // Weak: pruned by for_each_gc_cleanup after sweep.
            next_format_pointer_id: _,
            #[cfg(feature = "snapshot")]
                rust_fns_by_id: _, // Function pointers, not heap values.
            #[cfg(feature = "snapshot")]
                rust_fn_ids_by_addr: _,
            #[cfg(feature = "snapshot")]
            env_tokens,
            #[cfg(feature = "snapshot")]
            env_baselines,
            in_flight,
        } = self;
        let InFlight {
            stack_bottom: _, // An index into `stack`.
            call_stack,
            vararg_stack,
            open_upvalues,
            pending_bytecode_caches,
            rooted_values,
            metamethod_depth: _,
            call_depth: _,
            native_call_depth: _,
            active_rust_fn: _, // A function pointer, not a heap value.
        } = in_flight;

        visit(GcRoot::Stack(stack));
        visit(GcRoot::Globals(global_env));
        for (identity, entry) in bytecode_caches {
            debug_assert_eq!(*identity, Arc::as_ptr(&entry.bytecode) as usize);
            visit(GcRoot::ChunkLiterals(&entry.runtime.literals));
        }
        visit(GcRoot::MetaNames(meta_names));
        if let Some(mt) = string_metatable {
            visit(GcRoot::StringMetatable(*mt));
        }
        visit(GcRoot::Anchors(registry));
        visit(GcRoot::RootedValues(rooted_values));
        for env in suspended_envs {
            visit(GcRoot::SuspendedEnv(env));
        }
        visit(GcRoot::Varargs(vararg_stack));
        for (_, uv_ref) in open_upvalues {
            visit(GcRoot::OpenUpvalue(*uv_ref));
        }
        for call in call_stack {
            visit(GcRoot::ActiveChunk(&call.bytecode));
        }
        for identity in pending_bytecode_caches {
            visit(GcRoot::PendingChunk(*identity));
        }
        #[cfg(feature = "snapshot")]
        {
            for ptr in env_tokens.keys() {
                visit(GcRoot::EnvObject(*ptr));
            }
            for baseline in env_baselines.values() {
                visit(GcRoot::EnvBaseline(baseline));
            }
        }
    }

    /// Classify the State by the lifetime of what its fields hold. A new
    /// State field fails to compile here until it is named: in `InFlight`,
    /// as host-held, or bound to `_` as persistent with the reason.
    #[cfg(any(debug_assertions, feature = "snapshot"))]
    pub(super) fn rest_state(&self) -> RestState {
        let State {
            #[cfg(feature = "fault-injection")]
                fault_injection: _, // Test-only; disarmed before observing quiescence.
            global_env: _,
            string_metatable: _,
            string_mt_index_shape: _,
            meta_names: _,
            stack,
            heap: _,
            bytecode_caches: _,
            suspended_envs,
            upvalue_pool: _,
            cost: _,
            patterns: _,
            pattern_bytes: _,
            #[cfg(test)]
                pattern_compilations: _,
            poisoned: _, // Checked separately, before any rest-state question.
            frame_stacks,
            callbacks: _,
            user_data: _,
            rng: _,
            registry: _, // Host-owned handles; allowed in a snapshot, not saved.
            format_pointer_ids: _,
            next_format_pointer_id: _,
            #[cfg(feature = "snapshot")]
                rust_fns_by_id: _,
            #[cfg(feature = "snapshot")]
                rust_fn_ids_by_addr: _,
            #[cfg(feature = "snapshot")]
                env_tokens: _,
            #[cfg(feature = "snapshot")]
                env_baselines: _,
            in_flight,
        } = self;
        // The pool itself persists (capacity); only its stacks' frames are
        // execution-lifetime.
        if !in_flight.is_at_rest() || !frame_stacks.iter().all(Vec::is_empty) {
            RestState::Executing
        } else if !stack.is_empty() || !suspended_envs.is_empty() {
            RestState::HostHeld
        } else {
            RestState::Quiescent
        }
    }

    /// Return every execution-lifetime and host-held field to empty, for a
    /// State built by `load_state` (never one with a call in progress).
    #[cfg(feature = "snapshot")]
    pub(super) fn reset_in_flight(&mut self) {
        self.in_flight.clear();
        self.stack.clear();
        self.suspended_envs.clear();
    }
}

#[cfg(all(test, any(debug_assertions, feature = "snapshot")))]
mod tests {
    use super::*;
    use crate::vm::object::Closure;

    /// Load `source` and return a copy of the closure `load` pushed.
    fn loaded_closure(state: &mut State, source: &str) -> Closure {
        state
            .load(&crate::Program::compile(source, None).expect("test chunk compiles"))
            .expect("load");
        let Val::Obj(ptr) = state.stack[state.stack.len() - 1] else {
            unreachable!("load pushes a closure");
        };
        state
            .heap
            .as_lua_function_ref(ptr)
            .expect("loaded chunk is a closure")
            .clone()
    }

    #[test]
    fn a_new_in_flight_is_at_rest() {
        assert!(InFlight::new().is_at_rest());
    }

    /// Every field, set alone, takes the inventory out of rest, and `clear`
    /// brings it back without dropping capacity. The literal and the
    /// destructuring below have no rest pattern, so a new field fails to
    /// compile this test until it has a dirty value.
    #[cfg(feature = "snapshot")]
    #[test]
    fn each_in_flight_field_breaks_rest_and_clears() {
        fn noop(_: &mut State) -> crate::Result<u8> {
            Ok(0)
        }
        fn dirty() -> InFlight {
            let mut state = State::raw();
            let closure = loaded_closure(&mut state, "");
            InFlight {
                stack_bottom: 1,
                call_stack: vec![CallInfo {
                    bytecode: closure.bytecode,
                    ip: 0,
                    vararg_start: 0,
                    chain_charges: 0,
                }],
                vararg_stack: vec![Val::Nil],
                open_upvalues: vec![(0, UpvalueRef::new(0))],
                pending_bytecode_caches: vec![1],
                rooted_values: vec![Val::Nil],
                metamethod_depth: 1,
                call_depth: 1,
                native_call_depth: 1,
                active_rust_fn: Some(noop),
            }
        }
        fn check(mut single: InFlight) {
            assert!(!single.is_at_rest());
            let capacities = [
                single.call_stack.capacity(),
                single.vararg_stack.capacity(),
                single.open_upvalues.capacity(),
                single.pending_bytecode_caches.capacity(),
                single.rooted_values.capacity(),
            ];
            single.clear();
            assert!(single.is_at_rest());
            assert_eq!(
                [
                    single.call_stack.capacity(),
                    single.vararg_stack.capacity(),
                    single.open_upvalues.capacity(),
                    single.pending_bytecode_caches.capacity(),
                    single.rooted_values.capacity(),
                ],
                capacities
            );
        }
        let InFlight {
            stack_bottom,
            call_stack,
            vararg_stack,
            open_upvalues,
            pending_bytecode_caches,
            rooted_values,
            metamethod_depth,
            call_depth,
            native_call_depth,
            active_rust_fn,
        } = dirty();
        let clean = InFlight::new;
        check(InFlight {
            stack_bottom,
            ..clean()
        });
        check(InFlight {
            call_stack,
            ..clean()
        });
        check(InFlight {
            vararg_stack,
            ..clean()
        });
        check(InFlight {
            open_upvalues,
            ..clean()
        });
        check(InFlight {
            pending_bytecode_caches,
            ..clean()
        });
        check(InFlight {
            rooted_values,
            ..clean()
        });
        check(InFlight {
            metamethod_depth,
            ..clean()
        });
        check(InFlight {
            call_depth,
            ..clean()
        });
        check(InFlight {
            native_call_depth,
            ..clean()
        });
        check(InFlight {
            active_rust_fn,
            ..clean()
        });
    }

    #[test]
    fn host_held_fields_are_legal_at_rest_but_not_quiescent() {
        let mut state = State::new();
        assert_eq!(state.rest_state(), RestState::Quiescent);
        state.push_nil().expect("one value fits the stack");
        assert_eq!(state.rest_state(), RestState::HostHeld);
        state.pop(1).expect("the pushed value is visible");
        assert_eq!(state.rest_state(), RestState::Quiescent);
        state
            .with_restricted_env(&[], |state| {
                assert_eq!(state.rest_state(), RestState::HostHeld);
            })
            .expect("an empty callback cannot fail");
        assert_eq!(state.rest_state(), RestState::Quiescent);
    }

    #[test]
    fn a_pooled_frame_stack_holding_frames_is_executing() {
        let mut state = State::new();
        let closure = loaded_closure(&mut state, "return 1");
        state
            .call(crate::ArgCount::Fixed(0), crate::RetCount::Fixed(0))
            .expect("run");
        assert_eq!(state.rest_state(), RestState::Quiescent);
        assert!(
            !state.frame_stacks.is_empty(),
            "a call pools its frame stack"
        );
        state.frame_stacks[0].push(super::super::frame::Frame::new(
            closure.bytecode,
            closure.runtime,
            closure.upvalues,
            0,
        ));
        assert_eq!(state.rest_state(), RestState::Executing);
        state.frame_stacks[0].clear();
        assert_eq!(state.rest_state(), RestState::Quiescent);
    }
}
