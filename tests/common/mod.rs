//! Helpers shared by integration tests that spawn the CLI.
//!
//! The examples-corpus walker lives in `examples.rs` beside this file. Tests
//! that need it include it with `#[path = "common/examples.rs"]`, as they do
//! `cli.rs`, so test crates that never walk the corpus do not compile it as
//! dead code.

/// The `dellingr` binary Cargo built for this test run. Tests must use this
/// rather than a `target/` path, which may hold a stale binary from another
/// profile, checkout or commit.
#[cfg_attr(feature = "hotpath", allow(dead_code))]
pub(crate) fn dellingr_bin() -> &'static str {
    env!("CARGO_BIN_EXE_dellingr")
}
