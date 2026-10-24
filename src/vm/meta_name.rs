//! Metamethod event names, interned once per State.
//!
//! Every dispatch site that looks up an event in a metatable reads its key
//! from [`MetaNames`] instead of interning the spelling on demand. The names
//! are interned when the State is built and stay rooted for its lifetime (see
//! `GcRoot::MetaNames` in `vm/inventory.rs`), so a lookup never allocates, never collects and needs
//! no rooting of the values the caller holds.
//!
//! Because the names never die, a `%p` identity assigned to one of them is
//! stable across collections; the snapshot writer carries such identities
//! even when the saved graph does not otherwise reach the string.

use super::Val;
use super::object::{GcHeap, StringPtr};

/// A metamethod event the VM or the standard library dispatches on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetaName {
    Index,
    NewIndex,
    Call,
    Len,
    ToString,
    Metatable,
}

impl MetaName {
    pub(crate) const ALL: [MetaName; 6] = [
        MetaName::Index,
        MetaName::NewIndex,
        MetaName::Call,
        MetaName::Len,
        MetaName::ToString,
        MetaName::Metatable,
    ];

    /// The Lua spelling of the event key.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            MetaName::Index => "__index",
            MetaName::NewIndex => "__newindex",
            MetaName::Call => "__call",
            MetaName::Len => "__len",
            MetaName::ToString => "__tostring",
            MetaName::Metatable => "__metatable",
        }
    }
}

/// The State's interned event names, indexed by [`MetaName`].
pub(super) struct MetaNames([StringPtr; MetaName::ALL.len()]);

impl MetaNames {
    /// Intern every name without collecting. Called while the heap is being
    /// built, before anything else can need rooting.
    pub(super) fn intern(heap: &mut GcHeap) -> Self {
        Self(MetaName::ALL.map(|name| {
            heap.no_gc_allocator()
                .alloc_string(name.as_str().as_bytes())
        }))
    }

    /// The interned key for `name`, as a value ready for a table lookup.
    #[inline(always)]
    pub(super) fn get(&self, name: MetaName) -> Val {
        Val::Str(self.0[name as usize])
    }

    /// Every interned name, in [`MetaName::ALL`] order.
    #[cfg(feature = "snapshot")]
    pub(super) fn iter(&self) -> impl Iterator<Item = Val> + '_ {
        self.0.iter().map(|ptr| Val::Str(*ptr))
    }

    /// Strings have no children, so marking needs no worklist.
    pub(super) fn mark(&self, heap: &GcHeap) {
        for ptr in &self.0 {
            heap.mark_string(*ptr);
        }
    }
}
