//! Rust function identity.
//!
//! A Rust function value is a handle into its State's function table, never a
//! code address. Rust gives one function no single address: identical bodies
//! can be merged by the optimizer or the linker, and one function can be
//! instantiated in several codegen units. Address identity therefore made
//! `==`, table keys, rendered identities and saved function ids depend on how
//! the binary was built. Identity is the table entry instead: an entry is
//! created by an explicit call (`State::add_rust_fn`, or
//! `State::register_rust_fn` under the `snapshot` feature) or is one of the
//! standard library's catalog entries, and no function-pointer comparison
//! decides identity, registration acceptance or a dispatch fast path.
//!
//! The table is append-only and holds no heap values, so the collector never
//! visits it and no entry is ever invalidated. The standard library's
//! functions occupy fixed positions at the front of every State's table
//! (`lua_std::catalog_functions`), so the VM can recognise `next` and the
//! `ipairs` iterator by handle.

use std::num::NonZeroU64;

#[cfg(feature = "snapshot")]
use std::collections::BTreeMap;

use super::lua_val::RustFunc;

/// The index of one entry in a State's Rust function table: what a Lua value
/// holding a Rust function carries. Only meaningful in the State that issued
/// it. Equality and hashing are by index, which depends on the order entries
/// were created, so nothing may persist it or derive an order from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct RustFnId(u32);

impl RustFnId {
    /// The catalog entry at `index`. Catalog positions are fixed per build.
    pub(crate) const fn catalog(index: usize) -> Self {
        assert!(index < u32::MAX as usize, "the catalog index fits in u32");
        Self(index as u32)
    }

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// A host-held reference to a Rust function created by
/// [`State::add_rust_fn`](crate::State::add_rust_fn) (or
/// `State::register_rust_fn` under the `snapshot` feature). Push it with
/// [`State::push_rust_fn`](crate::State::push_rust_fn).
///
/// Each creation call makes a new function identity: two handles from two
/// calls name two functions that compare unequal from Lua, even for the same
/// Rust function. Pushing one handle any number of times gives equal values.
/// Create a handle once, during setup, and keep it; creating one per call
/// grows the State's function table for its whole life.
///
/// A handle is bound to the State that issued it. Using it with any other
/// State, including one rebuilt by `State::load_state` from a save of the
/// issuing State, returns [`ErrorKind::InvalidRustFnHandle`]: a loaded State's
/// setup obtains its own handles. Like [`Anchor`](crate::Anchor), its
/// `Debug` and `Hash` include a process-local State id.
///
/// [`ErrorKind::InvalidRustFnHandle`]: crate::error::ErrorKind::InvalidRustFnHandle
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RustFnHandle {
    state_id: NonZeroU64,
    id: RustFnId,
}

/// The entries of one State's Rust function table, with their stable save ids
/// under the `snapshot` feature.
pub(crate) struct RustFnTable {
    /// The issuing State's process-unique id, the same one its anchors carry.
    state_id: NonZeroU64,
    functions: Vec<RustFunc>,
    /// Each entry's stable save id, if it has one. A catalog entry gets its
    /// id when `open_libs` binds the catalog, a host entry when it is
    /// registered.
    #[cfg(feature = "snapshot")]
    ids: Vec<Option<Box<str>>>,
    /// Stable id to entry. An id names exactly one entry and an entry has at
    /// most one id. Iterated in id order only (conflict reporting), never in
    /// entry order.
    #[cfg(feature = "snapshot")]
    by_id: BTreeMap<Box<str>, RustFnId>,
}

impl RustFnTable {
    /// A table holding the standard-library catalog, unbound.
    pub(crate) fn new(state_id: NonZeroU64) -> Self {
        let functions: Vec<RustFunc> = crate::lua_std::catalog_functions().collect();
        Self {
            state_id,
            #[cfg(feature = "snapshot")]
            ids: vec![None; functions.len()],
            functions,
            #[cfg(feature = "snapshot")]
            by_id: BTreeMap::new(),
        }
    }

    /// The function an entry calls. Every `RustFnId` in a State was issued
    /// by its own table, which never shrinks.
    #[inline]
    pub(crate) fn function(&self, id: RustFnId) -> RustFunc {
        self.functions[id.index()]
    }

    /// The next entry's id, or `None` when the u32 index space is spent.
    fn next_id(&self) -> Option<RustFnId> {
        match u32::try_from(self.functions.len()) {
            Ok(index) if index < u32::MAX => Some(RustFnId(index)),
            _ => None,
        }
    }

    /// Append an entry without an id. On exhaustion nothing changes.
    pub(crate) fn add(&mut self, function: RustFunc) -> Option<RustFnHandle> {
        let id = self.next_id()?;
        self.functions.push(function);
        #[cfg(feature = "snapshot")]
        self.ids.push(None);
        Some(self.handle(id))
    }

    fn handle(&self, id: RustFnId) -> RustFnHandle {
        RustFnHandle {
            state_id: self.state_id,
            id,
        }
    }

    /// The entry a handle names, if this table issued it.
    pub(crate) fn resolve(&self, handle: RustFnHandle) -> Option<RustFnId> {
        (handle.state_id == self.state_id).then_some(handle.id)
    }

    /// Test observation: the id the next entry would get.
    #[cfg(all(test, feature = "snapshot"))]
    pub(crate) fn next_entry_for_test(&self) -> Option<RustFnId> {
        self.next_id()
    }

    /// The largest number of entries a table can hold.
    pub(crate) const CAPACITY: usize = u32::MAX as usize;
}

#[cfg(feature = "snapshot")]
impl RustFnTable {
    /// The stable save id of an entry, if it has one.
    pub(crate) fn save_id(&self, id: RustFnId) -> Option<&str> {
        self.ids[id.index()].as_deref()
    }

    /// The entry registered under a stable id.
    pub(crate) fn lookup(&self, id: &str) -> Option<RustFnId> {
        self.by_id.get(id).copied()
    }

    /// Whether `id` is already bound to an entry.
    pub(crate) fn is_bound(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    /// The handle of an entry, for a host lookup by id.
    pub(crate) fn handle_of(&self, id: RustFnId) -> RustFnHandle {
        self.handle(id)
    }

    /// Append an entry under a new stable id. The caller has checked that
    /// `id` is unbound and the id length; on exhaustion nothing changes.
    pub(crate) fn register(&mut self, id: &str, function: RustFunc) -> Option<RustFnHandle> {
        debug_assert!(!self.is_bound(id), "registration ids are checked unbound");
        let entry = self.next_id()?;
        self.functions.push(function);
        self.ids.push(Some(id.into()));
        self.by_id.insert(id.into(), entry);
        Some(self.handle(entry))
    }

    /// The smallest catalog id bound to an entry other than its own catalog
    /// entry, which `open_libs` must refuse before binding anything.
    pub(crate) fn catalog_conflict(&self) -> Option<String> {
        let mut conflict: Option<String> = None;
        for (entry, id) in crate::lua_std::catalog_ids() {
            if let Some(bound) = self.by_id.get(id.as_str())
                && *bound != entry
                && conflict.as_ref().is_none_or(|smallest| id < *smallest)
            {
                conflict = Some(id);
            }
        }
        conflict
    }

    /// Bind every catalog entry to its stable id. Idempotent; the caller has
    /// ruled out conflicts with `catalog_conflict`.
    pub(crate) fn bind_catalog(&mut self) {
        for (entry, id) in crate::lua_std::catalog_ids() {
            debug_assert!(
                self.by_id
                    .get(id.as_str())
                    .is_none_or(|bound| *bound == entry)
            );
            self.ids[entry.index()] = Some(id.as_str().into());
            self.by_id.insert(id.into_boxed_str(), entry);
        }
    }

    /// Stable id to entry, for resolving a save's function ids on load.
    pub(crate) fn ids(&self) -> &BTreeMap<Box<str>, RustFnId> {
        &self.by_id
    }
}
