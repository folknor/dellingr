//! Helpers shared by the suites of this crate.
//!
//! The files under `tests/common/` are also compiled by another target (the
//! native-stack test, the compiler's unit tests, the golden-regeneration
//! example), so they stay self-contained and are included here by path.

#[path = "../../common/examples.rs"]
pub(crate) mod examples;
mod lua;
pub(crate) mod process;
mod recorder;
#[cfg(feature = "snapshot")]
#[path = "../../common/save_golden_program.rs"]
pub(crate) mod save_golden_program;
pub(crate) mod scratch;
#[path = "../../common/stack.rs"]
pub(crate) mod stack;

pub(crate) use lua::*;
pub(crate) use recorder::Recorder;

/// The `dellingr` binary Cargo built for this test run. Tests must use this
/// rather than a `target/` path, which may hold a stale binary from another
/// profile, checkout or commit.
pub(crate) fn dellingr_bin() -> &'static str {
    env!("CARGO_BIN_EXE_dellingr")
}
