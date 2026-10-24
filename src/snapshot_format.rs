//! Persistence compatibility constants, available even without the codec.

/// Snapshot compatibility is exact-match only. Change this only when encoded
/// fields change, keeping opcode fixtures available in non-snapshot builds.
/// An unreleased version may evolve before shipping. Once released, encoding
/// changes require a new version; metadata and compiler output changes alone
/// do not. The release procedure and frozen-format checks are in
/// `docs/save-state.md`.
// Version 13: the global table is saved whole, with its pins, in place of the
// named bindings and the environment delta for `_G`.
#[cfg_attr(not(feature = "snapshot"), allow(dead_code))]
pub(crate) const FORMAT_VERSION: u16 = 13;
