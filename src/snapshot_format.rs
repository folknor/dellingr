//! Persistence compatibility constants, available even without the codec.

/// Snapshot compatibility is exact-match only. Change this only when encoded
/// fields change, keeping opcode fixtures available in non-snapshot builds.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub(crate) const FORMAT_VERSION: u16 = 7;
