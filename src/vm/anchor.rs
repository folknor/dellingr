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
use super::Error;
use super::ErrorKind;
use super::LuaType;
use super::Result;
use super::RetCount;
use super::State;
use super::TypeError;
use super::Val;
use super::object::{GcHeap, Markable, ObjectPtr};

new_key_type! {
    pub(crate) struct AnchorKey;
}

/// Process-wide allocator for `state_id`s. Each `State` gets a unique
/// `NonZeroU64` from this counter at construction; every `Anchor` carries
/// its owning State's id so cross-State misuse is detected. A `u64`
/// counter cannot wrap in any realistic process lifetime, so cross-State
/// protection is guaranteed even for long-running embedders.
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
        // Surface "stack empty" as an embedder error rather than the
        // VM-bug panic in pop_val. Validate before popping so the stack
        // is untouched on error - matches `anchor_at` and the rest of
        // the host API.
        let val = self.at_index(-1)?;
        if matches!(val, Val::Nil) {
            return Err(Error::without_location(ErrorKind::AnchorNil));
        }
        let anchor = self.registry.insert(val);
        self.pop_val();
        Ok(anchor)
    }

    /// Like `anchor`, but reads the value at `idx` without popping. The
    /// stack is left untouched on both success and error.
    pub fn anchor_at(&mut self, idx: isize) -> Result<Anchor> {
        let val = self.at_index(idx)?;
        if matches!(val, Val::Nil) {
            return Err(Error::without_location(ErrorKind::AnchorNil));
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
        match self.registry.get(a) {
            Some(val) => self.push_val(val),
            None => Err(Error::without_location(ErrorKind::InvalidAnchor)),
        }
    }

    /// Push the anchored value and call it. Convenience over
    /// `push_anchor` + `call`. Cost charges through the existing dispatch
    /// path; `anchor` and `release_anchor` themselves charge nothing.
    pub fn call_anchor(&mut self, a: Anchor, args: ArgCount, rets: RetCount) -> Result<()> {
        // The function must be pushed BEFORE the args are arranged on the
        // stack by the caller. Embedder protocol: push args, then
        // call_anchor (which inserts the function below the args).
        // Implementation: push the function on top, then rotate it under
        // the args.
        let val = match self.registry.get(a) {
            Some(val) => val,
            None => return Err(Error::without_location(ErrorKind::InvalidAnchor)),
        };
        let n_args = match args {
            ArgCount::Fixed(n) => n as usize,
            ArgCount::Dynamic => {
                // ArgCount::Dynamic relies on a vararg_call_base pushed by
                // OP_MARK_CALL_BASE inside bytecode; no public host API
                // populates that stack, so a host-side call_anchor with
                // Dynamic would either panic on the missing base or read
                // a stale one set up by prior bytecode and corrupt the
                // call. Reject it explicitly.
                return Err(Error::without_location(ErrorKind::InternalError(
                    "call_anchor does not support ArgCount::Dynamic; use ArgCount::Fixed".into(),
                )));
            }
        };
        // Insert the fn below the n_args topmost values.
        let insert_at = self.stack.len().checked_sub(n_args).ok_or_else(|| {
            Error::without_location(ErrorKind::InvalidStackIndex {
                index: -(n_args as isize) - 1,
            })
        })?;
        self.check_stack_space(1)?;
        self.stack.insert(insert_at, val);
        self.call(args, rets)
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
