//! Explicitly gated rewrite support for the checked-in save golden.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

// This test-only host reads the explicit opt-in that authorizes a fixture rewrite.
#[allow(clippy::disallowed_methods)]
fn require_regeneration_opt_in() {
    match std::env::var_os("DELLINGR_REGENERATE_GOLDEN") {
        Some(value) if value == OsStr::new("1") => {}
        _ => panic!("set DELLINGR_REGENERATE_GOLDEN=1 to rewrite the checked-in save fixture"),
    }
}

/// Rewrites the checked-in save golden only after a deliberate environment opt-in.
pub(crate) fn rewrite_save_golden_fixture(bytes: &[u8]) {
    require_regeneration_opt_in();

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/save_golden_current.bin");
    fs::write(fixture, bytes).expect("golden fixture is writable");
}
