//! The CLI binary Cargo built for this test run links no platform libm
//! transcendental (reference/deterministic-math.md).
//!
//! Clippy's bans cannot see dependencies or LLVM libcalls, so this inspects
//! the linked symbols instead, through `scripts/libm_symbols.py`. `brokkr
//! check` runs it once per ordinary `[[check]]` feature shape, which checks the
//! dev-profile binary of each shape, retaining coverage before release
//! optimization. The dedicated libm_symbols target checks the fat-LTO
//! binaries in both release sweeps. `support::dellingr_bin()` also verifies
//! that the CLI binary has the integration harness's snapshot feature shape.

#[path = "../../common/libm_symbols.rs"]
mod common;

#[test]
fn cli_binary_links_no_platform_libm_transcendental() {
    common::assert_cli_binary_links_no_platform_libm_transcendental(crate::support::dellingr_bin());
}
