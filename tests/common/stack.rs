/// Thread stack for tests that drive a recursion limit to its boundary: the
/// Rust default for spawned threads, and the native stack docs/limits.md
/// states every limit fits in, unoptimized builds included. Set explicitly so
/// `RUST_MIN_STACK` cannot loosen it.
#[cfg(not(any(
    feature = "hotpath",
    feature = "hotpath-alloc",
    feature = "fault-injection"
)))]
pub(crate) const LIMIT_TEST_STACK_BYTES: usize = 2 << 20;

/// `#[hotpath::measure]` wraps every annotated function in a timing guard,
/// which an instrumented build adds to each native level. The stack envelope
/// in docs/limits.md is stated for uninstrumented builds. Fault injection
/// also adds bookkeeping to preflights on these frames.
#[cfg(any(
    feature = "hotpath",
    feature = "hotpath-alloc",
    feature = "fault-injection"
))]
pub(crate) const LIMIT_TEST_STACK_BYTES: usize = 8 << 20;

/// Run `f` on a named thread with [`LIMIT_TEST_STACK_BYTES`] of stack. A
/// native stack overflow aborts the whole test process; the runtime's abort
/// message names the thread, which attributes it to the test.
#[allow(dead_code)]
pub(crate) fn on_limit_test_stack(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(LIMIT_TEST_STACK_BYTES)
        .spawn(f)
        .expect("spawning the test thread should succeed")
        .join()
        .expect("limit test thread should not panic");
}
