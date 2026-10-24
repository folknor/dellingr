//! The integration test crate: every suite that can share a process, grouped
//! by subject, so a focused run selects a subject or a file by its module
//! path (`brokkr test snapshot::`, `brokkr test gc_upvalues::`) and each sweep
//! links one binary instead of one per file.
//!
//! `tests/native_stack.rs` is the one other test target. It stays separate
//! because the release sweeps build it alone, optimized, by target name, and
//! because it drives the native stack to the edge of the documented envelope,
//! where an overflow aborts the whole process.
//!
//! The process-spawning suites (`cli`, the corpus runners) belong here too:
//! they change nothing in this process (working directories and resource
//! limits apply to their children). So does `host::panic_poison`, whose panic
//! hook forwards every panic but its own. Anything else that is process-wide
//! carries a `// process-global:` comment; `corpus::test_conventions` lists
//! the constructs it checks, and keeps the tree to these two targets with
//! every source reached from here. Cargo's `autotests` stays on, so a stray
//! top-level file still runs while that check fails, rather than silently
//! never compiling.
//!
//! The snapshot suites are gated on the `mod` line, so the default sweep
//! compiles none of them instead of running empty binaries.

mod cli;
mod compiler;
mod corpus;
mod cost;
mod failures;
mod host;
mod library;
mod runtime;
#[cfg(feature = "snapshot")]
mod snapshot;
mod support;
