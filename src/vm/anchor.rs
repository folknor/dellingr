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

use std::num::{NonZeroU32, NonZeroU64};
use std::sync::atomic::{AtomicU32, Ordering};

use slotmap::{Key, KeyData, SlotMap, new_key_type};

use super::Val;
use super::object::{GcHeap, Markable, UpvaluePool};

new_key_type! {
    pub(crate) struct AnchorKey;
}

/// Process-wide allocator for `state_id`s. Each `State` gets a unique
/// `NonZeroU32` from this counter at construction; every `Anchor` carries
/// its owning State's id so cross-State misuse is detected.
static NEXT_STATE_ID: AtomicU32 = AtomicU32::new(1);

pub(crate) fn next_state_id() -> NonZeroU32 {
    loop {
        let id = NEXT_STATE_ID.fetch_add(1, Ordering::Relaxed);
        if let Some(nz) = NonZeroU32::new(id) {
            return nz;
        }
        // Counter wrapped past zero; loop again.
    }
}

/// A retainable handle to a Lua value, valid until released.
///
/// `Anchor` is `Copy + Send + Sync + 'static`, 12 bytes on 64-bit targets.
/// `Option<Anchor>` is also 12 bytes via the `NonZero` niches.
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
    state_id: NonZeroU32,
    /// Slotmap `KeyData::as_ffi()` encoding of the (slot, generation) pair.
    /// `NonZero` because slotmap reserves zero for null keys.
    key: NonZeroU64,
}

impl Anchor {
    fn new(state_id: NonZeroU32, key: AnchorKey) -> Self {
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
    state_id: NonZeroU32,
    slots: SlotMap<AnchorKey, Val>,
}

impl Registry {
    pub(crate) fn new(state_id: NonZeroU32) -> Self {
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
}

impl Markable for Registry {
    fn mark_reachable(&self, heap: &GcHeap, upvalue_pool: &UpvaluePool) {
        for val in self.slots.values() {
            val.mark_reachable(heap, upvalue_pool);
        }
    }
}
