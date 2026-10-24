/// Thread stack for tests that parse at the syntax budget. Instrumented
/// builds differ in frame layout (the whole-parse and lexer measurements
/// stay; recursive descent helpers are unmeasured), so they get headroom;
/// the uninstrumented size enforces the parsing envelope in docs/limits.md
/// regardless of RUST_MIN_STACK.
pub(crate) const PARSE_TEST_STACK_BYTES: usize = if cfg!(feature = "hotpath") {
    8 << 20
} else {
    2 << 20
};
