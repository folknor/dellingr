/// Thread stack for integration tests that recurse to `MAX_CALL_DEPTH` or
/// `MAX_METAMETHOD_DEPTH`. Unoptimized builds use far more native stack per
/// Lua call than the 2MB default test thread allows. This is a tested
/// configuration, not a portable minimum; see docs/limits.md.
pub(crate) const DEEP_RECURSION_TEST_STACK_BYTES: usize = 64 << 20;
