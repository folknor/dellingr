//! Explicitly gated rewrite support for the checked-in save golden.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

fn regeneration_is_authorized(value: Option<&OsStr>) -> bool {
    matches!(value, Some(value) if value == OsStr::new("1"))
}

/// Rewrites the checked-in save golden only after a deliberate environment opt-in.
#[allow(clippy::disallowed_methods)]
pub(crate) fn rewrite_save_golden_fixture(bytes: &[u8]) {
    let authorization = std::env::var_os("DELLINGR_REGENERATE_GOLDEN");
    let fixture = save_golden_fixture_path();
    if let Err(error) = rewrite_save_golden_fixture_at(bytes, &fixture, authorization.as_deref()) {
        panic!("{error}");
    }
}

/// Resolves the checked-in save golden path for the regeneration helper.
pub(crate) fn save_golden_fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/save_golden_current.bin")
}

/// Rewrites a fixture after the exact regeneration opt-in value is supplied.
pub(crate) fn rewrite_save_golden_fixture_at(
    bytes: &[u8],
    fixture: &Path,
    authorization: Option<&OsStr>,
) -> Result<(), String> {
    if !regeneration_is_authorized(authorization) {
        return Err(
            "set DELLINGR_REGENERATE_GOLDEN=1 to rewrite the checked-in save fixture".into(),
        );
    }
    fs::write(fixture, bytes).map_err(|error| format!("golden fixture is not writable: {error}"))
}
