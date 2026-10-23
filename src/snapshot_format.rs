//! Persistence compatibility constants, available even without the codec.

/// Snapshot compatibility is exact-match only. Change this only when encoded
/// fields change, keeping opcode fixtures available in non-snapshot builds.
// Version 12 covers both the environment-delta hash-slot history and the
// canonical live-span environment arrays: the second changed the encoding
// before any release had shipped version 12, so it shares the number.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub(crate) const FORMAT_VERSION: u16 = 12;
