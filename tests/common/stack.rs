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

/// Thread stack for tests that parse at the syntax budget: 2 MiB in every
/// repository profile, the stricter promise docs/limits.md keeps for parsing
/// on its own. Instrumented builds wrap parser functions in timing guards and
/// get more, as `src/compiler/parser.rs`'s unit-test constant does.
#[cfg(not(any(feature = "hotpath", feature = "hotpath-alloc")))]
pub(crate) const PARSE_TEST_STACK_BYTES: usize = 2 << 20;
#[cfg(any(feature = "hotpath", feature = "hotpath-alloc"))]
pub(crate) const PARSE_TEST_STACK_BYTES: usize = 8 << 20;

/// Run `f` on a named thread with [`LIMIT_TEST_STACK_BYTES`] of stack. A
/// native stack overflow aborts the whole test process; the runtime's abort
/// message names the thread, which attributes it to the test.
#[allow(dead_code)]
pub(crate) fn on_limit_test_stack(name: &str, f: impl FnOnce() + Send + 'static) {
    on_stack(name, LIMIT_TEST_STACK_BYTES, f);
}

/// Run `f` on a named thread with [`PARSE_TEST_STACK_BYTES`] of stack.
#[allow(dead_code)]
pub(crate) fn on_parse_test_stack(name: &str, f: impl FnOnce() + Send + 'static) {
    on_stack(name, PARSE_TEST_STACK_BYTES, f);
}

#[allow(dead_code)]
fn on_stack(name: &str, bytes: usize, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(bytes)
        .spawn(f)
        .expect("spawning the test thread should succeed")
        .join()
        .expect("limit test thread should not panic");
}
