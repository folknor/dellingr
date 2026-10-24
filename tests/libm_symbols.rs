//! Separate target so release sweeps check their linked CLI without compiling
//! the full integration suite. Cargo supplies the sweep's own binary path.

#[path = "common/libm_symbols.rs"]
mod libm_symbols;

#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "dev sweeps check this CLI in corpus::libm_symbols"
)]
fn cli_binary_links_no_platform_libm_transcendental() {
    libm_symbols::assert_cli_binary_links_no_platform_libm_transcendental(env!(
        "CARGO_BIN_EXE_dellingr"
    ));
}
