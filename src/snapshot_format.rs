//! Persistence compatibility constants, available even without the codec.

/// Snapshot compatibility is exact-match only. Change this only when encoded
/// fields change, keeping opcode fixtures available in non-snapshot builds.
// Version 13: the global table is saved whole, with its pins, in place of the
// named bindings and the `_G` proxy's environment delta.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub(crate) const FORMAT_VERSION: u16 = 13;
