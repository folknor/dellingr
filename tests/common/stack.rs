// Included by path from `tests/native_stack.rs` and the integration crate's
// support module; every item here is used by both, so neither crate needs a
// dead-code allowance for it.

/// Thread stack for tests that drive a runtime recursion limit to its
/// boundary: the native stack docs/limits.md states every runtime limit fits
/// in, 4 MiB with this repository's dev settings and 2 MiB, the Rust default
/// for spawned threads, with its release settings. `cfg(debug_assertions)`
/// tells the two repository profiles apart here; it is not a definition of
/// "unoptimized". Set explicitly so `RUST_MIN_STACK` cannot loosen it.
#[cfg(not(any(
    feature = "hotpath",
    feature = "hotpath-alloc",
    feature = "fault-injection"
)))]
pub(crate) const LIMIT_TEST_STACK_BYTES: usize = if cfg!(debug_assertions) {
    4 << 20
} else {
    2 << 20
};

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
pub(crate) fn on_limit_test_stack(name: &str, f: impl FnOnce() + Send + 'static) {
    on_stack(name, LIMIT_TEST_STACK_BYTES, f);
}

/// Run `f` on a named thread with `bytes` of stack.
pub(crate) fn on_stack(name: &str, bytes: usize, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(bytes)
        .spawn(f)
        .expect("spawning the test thread should succeed")
        .join()
        .expect("limit test thread should not panic");
}
