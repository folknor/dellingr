//! The CLI binary Cargo built for this test run links no platform libm
//! transcendental (reference/deterministic-math.md).
//!
//! Clippy's bans cannot see dependencies or LLVM libcalls, so this inspects
//! the linked symbols instead, through `scripts/libm_symbols.py`. `brokkr
//! check` runs it once per ordinary `[[check]]` feature shape, which checks the
//! dev-profile binary of each shape. The fat-LTO release binary is not built by
//! those sweeps; `scripts/check_libm_symbols.sh` checks it, run by hand outside
//! `brokkr check`.

use std::path::Path;
use std::process::Command;

use crate::support::dellingr_bin;

#[test]
fn cli_binary_links_no_platform_libm_transcendental() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/libm_symbols.py");
    let output = Command::new("python3")
        .arg("-B")
        .arg(&script)
        .arg(dellingr_bin())
        .output()
        .expect("python3 should be available to inspect the binary's symbols");
    assert!(
        output.status.success(),
        "libm symbol check failed for {}:\nstdout: {}\nstderr: {}",
        dellingr_bin(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
