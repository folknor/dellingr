//! Check a linked CLI binary for platform libm transcendental symbols.

use std::path::Path;

pub(crate) fn assert_cli_binary_links_no_platform_libm_transcendental(binary: &str) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/libm_symbols.py");
    // process-spawn-exception: one of the two python3 spawns brokkr.toml's
    // test-processes-through-support rule names; whether it stays is undecided.
    let output = std::process::Command::new("python3")
        .arg("-B")
        .arg(&script)
        .arg(binary)
        .output()
        .expect("python3 should be available to inspect the binary's symbols");
    assert!(
        output.status.success(),
        "libm symbol check failed for {binary}:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
