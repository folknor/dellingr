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
//!
//! A catalog entry's name and id are compile-time strings (the catalog's
//! `&'static str` ids), which the table borrows; only host entries own their
//! name and id. Building a State therefore copies no catalog strings, which
//! matters to a host that builds a State per script or per tick.

use std::borrow::Cow;
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

    /// Whether this is a standard-library catalog entry: dellingr's own
    /// code, as opposed to a host function.
    #[inline(always)]
    pub(crate) fn is_library(self) -> bool {
        (self.0 as usize) < crate::lua_std::CATALOG_LEN
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
    /// Each entry's diagnostic name, the one argument errors raised while it
    /// runs carry: its catalog id for a standard-library entry, the name the
    /// host gave for a host entry. Fixed at creation, not unique, not saved,
    /// and never part of identity or saveability. Borrowed for a catalog
    /// entry, owned for a host entry.
    names: Vec<Cow<'static, str>>,
    /// Each entry's stable save id, if it has one. A catalog entry gets its
    /// id when `open_libs` binds the catalog, a host entry when it is
    /// registered: the catalog's ids being static does not make them bound,
    /// so a raw State's host may register one first (and `open_libs` then
    /// refuses). Borrowed for a catalog entry, owned for a host entry.
    #[cfg(feature = "snapshot")]
    ids: Vec<Option<Cow<'static, str>>>,
    /// Stable id to entry, the one source of truth for resolving an id. An
    /// id names exactly one entry and an entry has at most one id. Iterated
    /// in id order only (conflict reporting), never in entry order.
    #[cfg(feature = "snapshot")]
    by_id: BTreeMap<Cow<'static, str>, RustFnId>,
}

impl RustFnTable {
    /// A table holding the standard-library catalog, unbound.
    pub(crate) fn new(state_id: NonZeroU64) -> Self {
        let functions: Vec<RustFunc> = crate::lua_std::catalog_functions().collect();
        let names: Vec<Cow<'static, str>> = crate::lua_std::catalog_ids()
            .map(|(_, id)| Cow::Borrowed(id))
            .collect();
        debug_assert_eq!(names.len(), functions.len());
        Self {
            state_id,
            #[cfg(feature = "snapshot")]
            ids: vec![None; functions.len()],
            functions,
            names,
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

    /// The diagnostic name of an entry.
    pub(crate) fn name(&self, id: RustFnId) -> &str {
        &self.names[id.index()]
    }

    /// Append an entry named `name`, without an id. The caller has checked
    /// the name's size; on exhaustion nothing changes.
    pub(crate) fn add(&mut self, name: &str, function: RustFunc) -> Option<RustFnHandle> {
        let id = self.next_id()?;
        self.functions.push(function);
        self.names.push(Cow::Owned(name.to_owned()));
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

    /// Append an entry named `name` under a new stable id. The caller has
    /// checked that `id` is unbound and the id and name sizes; on exhaustion
    /// nothing changes.
    pub(crate) fn register(
        &mut self,
        id: &str,
        name: &str,
        function: RustFunc,
    ) -> Option<RustFnHandle> {
        debug_assert!(!self.is_bound(id), "registration ids are checked unbound");
        let entry = self.next_id()?;
        self.functions.push(function);
        self.names.push(Cow::Owned(name.to_owned()));
        self.ids.push(Some(Cow::Owned(id.to_owned())));
        self.by_id.insert(Cow::Owned(id.to_owned()), entry);
        Some(self.handle(entry))
    }

    /// The smallest catalog id bound to an entry other than its own catalog
    /// entry, which `open_libs` must refuse before binding anything.
    pub(crate) fn catalog_conflict(&self) -> Option<&'static str> {
        let mut conflict: Option<&'static str> = None;
        for (entry, id) in crate::lua_std::catalog_ids() {
            if let Some(bound) = self.by_id.get(id)
                && *bound != entry
                && conflict.is_none_or(|smallest| id < smallest)
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
            debug_assert!(self.by_id.get(id).is_none_or(|bound| *bound == entry));
            self.ids[entry.index()] = Some(Cow::Borrowed(id));
            self.by_id.insert(Cow::Borrowed(id), entry);
        }
    }

    /// Stable id to entry, for resolving a save's function ids on load.
    pub(crate) fn ids(&self) -> &BTreeMap<Cow<'static, str>, RustFnId> {
        &self.by_id
    }
}
