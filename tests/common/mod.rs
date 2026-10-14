//! Helpers shared by integration tests that spawn the CLI.

/// The `dellingr` binary Cargo built for this test run. Tests must use this
/// rather than a `target/` path, which may hold a stale binary from another
/// profile, checkout or commit.
pub(crate) fn dellingr_bin() -> &'static str {
    env!("CARGO_BIN_EXE_dellingr")
}
