//! The crate's deterministic hash collection types.
//!
//! We disable `indexmap`'s `std` feature, so its `S = RandomState` default
//! does not exist in standalone builds and a bare `indexmap::IndexMap<K, V>`
//! fails to compile. An embedding host can re-enable `std` through Cargo
//! feature unification; this alias still fixes the hasher explicitly.
//! Every map and set in the crate goes through these aliases, except the
//! string interner's `hashbrown::HashTable`, which is never iterated and
//! hashes with [`hash_bytes`] from this module. Iteration order
//! is insertion order regardless of the hasher, but the hasher still decides
//! bucket layout and probe counts, so an OS-seeded one makes performance
//! vary run to run. `FixedState` is foldhash with a compile-time constant
//! seed: identical hashes for identical inputs on the same target, without
//! process entropy, and cheaper than SipHash on the table hash part, which
//! is the VM's hottest lookup.
//!
//! Hash values are not a stable contract (they may change with a foldhash
//! upgrade, target architecture, or endianness), so nothing may persist them
//! or let them reach program-visible order. `Val::RustFn` keys hash by their
//! State's function-table index, which follows the order entries were
//! created; that only affects layout, never order.

/// Deterministic, fixed-seed hasher used by the crate's map and set aliases.
pub(crate) type FixedState = foldhash::fast::FixedState;

/// Insertion-ordered map with the crate's fixed-seed hasher. Construct with
/// `IndexMap::default()` or `IndexMap::with_capacity_and_hasher(n,
/// FixedState::default())`.
pub(crate) type IndexMap<K, V> = indexmap::IndexMap<K, V, FixedState>;

/// Insertion-ordered set with the crate's fixed-seed hasher.
pub(crate) type IndexSet<T> = indexmap::IndexSet<T, FixedState>;

/// `IndexMap::with_capacity` for the crate alias.
#[inline]
pub(crate) fn map_with_capacity<K, V>(capacity: usize) -> IndexMap<K, V> {
    IndexMap::with_capacity_and_hasher(capacity, FixedState::default())
}

/// Hash a byte string with the crate's fixed-seed hasher.
#[inline]
pub(crate) fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::BuildHasher;
    FixedState::default().hash_one(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::BuildHasher;

    #[test]
    fn fixed_state_hashes_identically_across_instances() {
        // A fresh builder must reproduce the same hash within this process.
        // This alone cannot detect a randomly initialized process-wide seed.
        let a = FixedState::default().hash_one(0x1234_5678_u64);
        let b = FixedState::default().hash_one(0x1234_5678_u64);
        assert_eq!(a, b);
    }
}
