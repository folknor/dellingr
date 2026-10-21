//! A registry for retainable Lua values.
//!
//! Embedders use [`Anchor`] handles to keep references to Lua values alive
//! across multiple host calls without polluting the script's global
//! namespace. Anchored values are GC roots: as long as an anchor is live,
//! the value it points at survives collection.
//!
//! Each anchor is bound to the [`State`](super::State) that produced it,
//! caught by an embedded `state_id`. Cross-State misuse returns
//! [`ErrorKind::InvalidAnchor`](super::ErrorKind::InvalidAnchor) instead
//! of silently hitting an unrelated value. The slotmap backing the
//! registry uses generational keys, so use-after-release is also caught
//! explicitly rather than silently aliasing a recycled slot.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use slotmap::{Key, KeyData, SlotMap, new_key_type};

use super::ArgCount;
use super::ErrorKind;
use super::LuaType;
use super::Result;
use super::RetCount;
use super::State;
use super::TypeError;
use super::Val;
use super::object::{GcHeap, Markable, ObjectPtr};
use crate::error::InternalFault;

new_key_type! {
    pub(crate) struct AnchorKey;
}

/// Process-wide allocator for `state_id`s. Each `State` gets a unique
/// `NonZeroU64` from this counter at construction; every `Anchor` carries
/// its owning State's id so cross-State misuse is detected. A `u64`
/// counter cannot wrap in any realistic process lifetime, so cross-State
/// protection is guaranteed even for long-running embedders.
///
/// This is deliberately process-global: ids must be unique across every State
/// in the process for the cross-State check to work. The cost is that State
/// creation order (including across threads, and the scratch State the stdlib
/// registry check builds) is observable through an `Anchor`'s `Debug` and
/// `Hash`. That is accepted: the id never reaches scripts, snapshots, the
/// RNG or the cost meter, so no replay-visible behavior depends on it. Hosts
/// must not persist or compare anchors across processes.
static NEXT_STATE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_state_id() -> NonZeroU64 {
    let id = NEXT_STATE_ID.fetch_add(1, Ordering::Relaxed);
    NonZeroU64::new(id).expect("u64 state-id counter cannot wrap in a real process")
}

/// A retainable handle to a Lua value, valid until released.
///
/// `Anchor` is `Copy + Send + Sync + 'static`, 16 bytes on 64-bit targets.
/// `Option<Anchor>` is also 16 bytes via the `NonZero` niches.
///
/// Anchors are bound to one `State`. Operations on a wrong-State anchor
/// return [`ErrorKind::InvalidAnchor`](super::ErrorKind::InvalidAnchor)
/// rather than silently aliasing into the wrong State's registry.
///
/// The internal bit representation embeds a process-allocator-derived
/// `state_id`, which is *not* deterministic across hosts; treat
/// `Anchor`'s `Debug` output the same way you would a memory address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    state_id: NonZeroU64,
    /// Slotmap `KeyData::as_ffi()` encoding of the (slot, generation) pair.
    /// `NonZero` because slotmap reserves zero for null keys.
    key: NonZeroU64,
}

impl Anchor {
    fn new(state_id: NonZeroU64, key: AnchorKey) -> Self {
        let ffi = key.data().as_ffi();
        let key = NonZeroU64::new(ffi).expect("real slotmap keys are never zero");
        Self { state_id, key }
    }

    fn slotmap_key(self) -> AnchorKey {
        AnchorKey::from(KeyData::from_ffi(self.key.get()))
    }
}

/// Per-`State` registry of retained values.
///
/// The internal `Cells` of `Val` aside, `Registry` is itself plain data:
/// a `state_id` plus a `SlotMap`. Generational keys give us
/// stale-handle detection for free, and slotmap iteration order is
/// deterministic given identical insert/release history.
// Host-owned handles intentionally have no automatic eviction: dropping an
// entry would invalidate a live Anchor and remove a GC root. The host bounds
// its own anchor count by release_anchor(). This is not GC-managed allocation storage
// and must not enter heap_size(), which drives existing GC threshold policy.
pub(crate) struct Registry {
    state_id: NonZeroU64,
    slots: SlotMap<AnchorKey, Val>,
}

impl Registry {
    pub(crate) fn new(state_id: NonZeroU64) -> Self {
        Self {
            state_id,
            slots: SlotMap::with_key(),
        }
    }

    pub(crate) fn insert(&mut self, value: Val) -> Anchor {
        let key = self.slots.insert(value);
        Anchor::new(self.state_id, key)
    }

    pub(crate) fn get(&self, a: Anchor) -> Option<Val> {
        if a.state_id != self.state_id {
            return None;
        }
        self.slots.get(a.slotmap_key()).copied()
    }

    pub(crate) fn remove(&mut self, a: Anchor) -> bool {
        if a.state_id != self.state_id {
            return false;
        }
        self.slots.remove(a.slotmap_key()).is_some()
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }
}

impl Markable for Registry {
    fn mark_reachable(&self, heap: &GcHeap, worklist: &mut Vec<ObjectPtr>) {
        for val in self.slots.values() {
            val.mark_reachable(heap, worklist);
        }
    }
}

impl State {
    /// Pop the top of stack and store it in this State's registry. Returns a
    /// `Copy` `Anchor` handle the embedder can store and use later to push
    /// or call the value.
    ///
    /// Errors with `ErrorKind::AnchorNil` if the top of stack is `nil`
    /// (use `Option<Anchor>` for an absent-value sentinel; nil carries no
    /// GC weight and has no use case for a stable handle). The stack is
    /// only popped on success - errors leave it untouched.
    pub fn anchor(&mut self) -> Result<Anchor> {
        self.check_poisoned()?;
        // Surface "stack empty" as an embedder error rather than the
        // VM-bug panic in pop_val. Validate before popping so the stack
        // is untouched on error - matches `anchor_at` and the rest of
        // the host API.
        let val = self.at_index(-1)?;
        if matches!(val, Val::Nil) {
            return Err(self.error(ErrorKind::AnchorNil));
        }
        let anchor = self.registry.insert(val);
        self.pop_val();
        Ok(anchor)
    }

    /// Like `anchor`, but reads the value at `idx` without popping. The
    /// stack is left untouched on both success and error.
    pub fn anchor_at(&mut self, idx: isize) -> Result<Anchor> {
        self.check_poisoned()?;
        let val = self.at_index(idx)?;
        if matches!(val, Val::Nil) {
            return Err(self.error(ErrorKind::AnchorNil));
        }
        Ok(self.registry.insert(val))
    }

    /// Like `anchor`, but additionally requires the value to be a function
    /// (Lua closure or `RustFunc`). Use this when you want the type error
    /// at registration time rather than at the first `call_anchor`.
    ///
    /// This is strict: tables with `__call` are rejected. To anchor a
    /// callable table, use `anchor` and let the existing dispatch handle
    /// `__call` at call time.
    pub fn anchor_function(&mut self) -> Result<Anchor> {
        self.check_poisoned()?;
        let val = self.at_index(-1)?;
        let typ = val.typ(&self.heap);
        if typ != LuaType::Function {
            return Err(self.type_error(TypeError::FunctionCall(typ)));
        }
        let anchor = self.registry.insert(val);
        self.pop_val();
        Ok(anchor)
    }

    /// Like `anchor_at`, but additionally requires the value to be a
    /// function. See `anchor_function` for the strictness note.
    pub fn anchor_function_at(&mut self, idx: isize) -> Result<Anchor> {
        self.check_poisoned()?;
        let val = self.at_index(idx)?;
        let typ = val.typ(&self.heap);
        if typ != LuaType::Function {
            return Err(self.type_error(TypeError::FunctionCall(typ)));
        }
        Ok(self.registry.insert(val))
    }

    /// Push the anchored value onto the stack. Errors with
    /// `ErrorKind::InvalidAnchor` if the handle is stale, released, or
    /// belongs to a different `State`.
    pub fn push_anchor(&mut self, a: Anchor) -> Result<()> {
        self.check_poisoned()?;
        match self.registry.get(a) {
            Some(val) => self.push_val(val),
            None => Err(self.error(ErrorKind::InvalidAnchor)),
        }
    }

    /// Push the anchored value and call it. Convenience over
    /// `push_anchor` + `call`. Cost charges through the existing dispatch
    /// path; `anchor` and `release_anchor` themselves charge nothing.
    ///
    /// # Errors
    ///
    /// Returns `InvalidStackIndex` without changing the stack if the current
    /// frame has fewer than the requested arguments. `ArgCount::Dynamic` is
    /// reserved for bytecode and is rejected without changing the stack.
    ///
    /// Like [`State::call`], errors raised before dispatch (a dead anchor,
    /// too few arguments, `Dynamic`, no stack room for the callee) are host
    /// API misuse: they are returned but do not fire
    /// [`HostCallbacks::on_error`](crate::HostCallbacks::on_error). Errors
    /// from the dispatched call notify exactly as for `call`.
    ///
    /// Guarded like [`State::call`]: refused on a poisoned State.
    pub fn call_anchor(&mut self, a: Anchor, args: ArgCount, rets: RetCount) -> Result<()> {
        self.guarded(|state| state.call_anchor_unguarded(a, args, rets))
    }

    fn call_anchor_unguarded(&mut self, a: Anchor, args: ArgCount, rets: RetCount) -> Result<()> {
        // Hosts push only the arguments. Insert the anchored function below
        // them to provide call's callee-then-arguments stack layout.
        //
        // The preflight errors below are built with `self.error`, as `call`'s
        // are, and deliberately not passed to `host_error`: `on_error` covers
        // execution, and a misuse failure has executed nothing (see the
        // `on_error` doc in host.rs).
        let val = match self.registry.get(a) {
            Some(val) => val,
            None => return Err(self.error(ErrorKind::InvalidAnchor)),
        };
        let n_args = match args {
            ArgCount::Fixed(n) => n as usize,
            ArgCount::Dynamic => {
                // ArgCount::Dynamic is resolved by the executing Lua frame
                // against a call base its own OP_MARK_CALL_BASE pushed; a
                // host owns no such base. Reject it explicitly, as
                // State::call does.
                return Err(super::Error::internal_fault(
                    InternalFault::CallAnchorDynamicArguments,
                ));
            }
        };
        // Insert the fn below the n_args topmost values. Validate against the
        // visible frame (get_top), not the absolute stack: inside a RustFunc
        // an absolute check would let insert_at land below stack_bottom and
        // shift the calling Lua frame's registers, and `call` would then fail
        // with the inserted value left behind. Check before any mutation.
        if n_args > self.get_top() {
            return Err(self.error(ErrorKind::InvalidStackIndex {
                index: -(n_args as isize) - 1,
            }));
        }
        let insert_at = self.stack.len() - n_args;
        self.check_stack_space(1)?;
        self.stack.insert(insert_at, val);
        self.call_unguarded(args, rets)
    }

    /// Release an anchor. Returns `true` if the handle was live and the
    /// value was freed; `false` for stale, already-released, or
    /// wrong-State handles. Idempotent: never errors, never panics.
    pub fn release_anchor(&mut self, a: Anchor) -> bool {
        self.registry.remove(a)
    }

    /// Returns the type of an anchored value. `None` if the handle is
    /// not live in this State.
    pub fn anchor_type(&self, a: Anchor) -> Option<LuaType> {
        self.registry.get(a).map(|val| val.typ(&self.heap))
    }

    /// Number of live anchors. For embedder leak diagnostics.
    pub fn anchor_count(&self) -> usize {
        self.registry.len()
    }
}
